//! Capturing the audio of one application (e.g. Spotify).
//!
//! Windows 10 version 2004 and later can capture the audio of a single process
//! (and its child processes) with "process loopback". So only the chosen app's
//! sound is shared; friends' voices and notification sounds aren't mixed in.
//! Other operating systems aren't supported yet.
//!
//! Structure of this file: the public API at the top works on every OS; the
//! Windows-only implementation lives in the private `imp` module at the bottom,
//! which is compiled only on Windows (`#[cfg(windows)]`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// An application that is playing audio.
#[derive(Clone, Debug)]
pub struct AppAudio {
    /// Process id.
    pub pid: u32,
    /// Display name, e.g. "Spotify".
    pub name: String,
}

/// We always capture 48 kHz stereo (Windows converts the app's audio to this).
/// `48_000`: underscores in number literals are just for readability.
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;

/// Is app-audio sharing available on this OS?
pub fn supported() -> bool {
    cfg!(windows)
}

/// Handle for a running capture; capturing stops when it's dropped.
pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

// On Windows, re-export the real `list` from the `imp` module;
// elsewhere provide a stub that returns an empty list.
#[cfg(windows)]
pub use imp::list;

#[cfg(not(windows))]
pub fn list() -> Vec<AppAudio> {
    Vec::new()
}

/// Whose audio to capture.
#[derive(Clone, Copy, Debug)]
// Outside Windows the pid inside `App` is never read; silence that warning there.
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Target {
    /// One app (process id), including the processes it started.
    App(u32),
    /// Everything playing on the computer *except* goodbyedc itself. Used when a
    /// whole screen is shared: the others hear the game/video, but not their own
    /// voices coming back from our speakers.
    AllButUs,
}

/// Captures the audio of `target` and hands 48 kHz stereo 32-bit float
/// samples (-1..1, interleaved) to `on_data`. Float is used so that quiet audio
/// that gets boosted later (normalize.rs) doesn't lose resolution.
///
/// `on_data: impl FnMut(&[f32]) + Send + 'static` means "any closure that:
///   - can be called repeatedly and may change its own captured state (FnMut),
///   - can be sent to another thread (Send),
///   - doesn't borrow short-lived data ('static)".
/// It will be called on the capture thread.
pub fn capture(target: Target, on_data: impl FnMut(&[f32]) + Send + 'static) -> anyhow::Result<Capture> {
    let stop = Arc::new(AtomicBool::new(false));
    // Only one of these two blocks exists in the compiled program, and it is the
    // last expression of the function, so its value is what `capture` returns.
    #[cfg(windows)]
    {
        imp::start(target, stop.clone(), on_data)?;
        Ok(Capture { stop })
    }
    #[cfg(not(windows))]
    {
        // Mark the parameters as used to avoid "unused variable" warnings.
        let _ = (target, on_data, stop);
        anyhow::bail!("Uygulama sesi paylaşımı şimdilik yalnızca Windows'ta çalışıyor")
    }
}

/// Windows implementation (COM / WASAPI). Almost everything here is `unsafe`
/// because it calls Windows C APIs that the Rust compiler can't check.
#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    // `Context` adds `.context("message")` to Results, wrapping the error with an
    // explanation.
    use anyhow::{anyhow, Context};
    use windows::core::{implement, Interface, IUnknown, HRESULT, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, BOOL};
    use windows::Win32::System::Threading::CreateEventW;
    // `*` imports everything from the module (many audio types are needed).
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IAgileObject, IAgileObject_Impl, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // `super::` refers to the parent module (the top of this file).
    use super::{AppAudio, CHANNELS, SAMPLE_RATE};

    /// Apps that have an audio session on any active speaker device.
    pub fn list() -> Vec<AppAudio> {
        // Run on a separate thread so COM initialization doesn't affect the caller.
        // `unwrap_or_default()` turns any error into an empty list.
        std::thread::spawn(|| unsafe { list_inner().unwrap_or_default() }).join().unwrap_or_default()
    }

    /// `unsafe fn`: the whole function body may call unsafe APIs, and callers must
    /// call it inside an `unsafe` block.
    unsafe fn list_inner() -> windows::core::Result<Vec<AppAudio>> {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let me = std::process::id();
        let mut out: Vec<AppAudio> = Vec::new();
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for d in 0..devices.GetCount()? {
            let Ok(device) = devices.Item(d) else { continue };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
            let Ok(sessions) = manager.GetSessionEnumerator() else { continue };
            for i in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(s) = sessions.GetSession(i) else { continue };
                let Ok(s2) = s.cast::<IAudioSessionControl2>() else { continue };
                // Skip Windows' own system sounds session.
                if s2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    continue;
                }
                let Ok(pid) = s2.GetProcessId() else { continue };
                // Skip "no process", ourselves, and duplicates.
                if pid == 0 || pid == me || out.iter().any(|a| a.pid == pid) {
                    continue;
                }
                let Some(name) = process_name(pid) else { continue };
                // Apps like browsers have many processes; show each name once.
                if out.iter().any(|a| a.name == name) {
                    continue;
                }
                out.push(AppAudio { pid, name });
            }
        }
        // Sort alphabetically, ignoring case.
        out.sort_by_key(|a| a.name.to_lowercase());
        Ok(out)
    }

    /// "C:\...\Spotify.exe" -> "Spotify" (first letter capitalized).
    unsafe fn process_name(pid: u32) -> Option<String> {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        // Windows APIs use UTF-16 strings, i.e. arrays of u16.
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        // `&buf[..len]` is a slice of the first `len` elements.
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&path).file_stem()?.to_string_lossy().to_string();
        let mut c = stem.chars();
        Some(match c.next() {
            // `+` on a String appends a `&str`.
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => stem,
        })
    }

    /// A COM object implemented in Rust. `ActivateAudioInterfaceAsync` is
    /// asynchronous: Windows calls `ActivateCompleted` on this object when it's
    /// done, and we forward that signal through a channel.
    ///
    /// The `#[implement(...)]` macro generates the COM boilerplate. Windows
    /// requires the handler to be "agile" (callable from any thread), hence
    /// `IAgileObject`. The sender is wrapped in `Mutex<Option<...>>` so it can be
    /// taken out and used exactly once from a `&self` method.
    #[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
    struct Handler(Mutex<Option<mpsc::Sender<()>>>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Handler {
        fn ActivateCompleted(
            &self,
            _op: Option<&IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            // `take()` moves the value out of the Option, leaving `None` behind.
            if let Some(tx) = self.0.lock().unwrap().take() {
                let _ = tx.send(());
            }
            Ok(())
        }
    }

    // IAgileObject has no methods; an empty impl is enough.
    impl IAgileObject_Impl for Handler {}

    /// Starts a capture thread for `target`. Returns once capturing is running (or
    /// with the error that prevented it).
    pub fn start(
        target: super::Target,
        stop: Arc<AtomicBool>,
        mut on_data: impl FnMut(&[f32]) + Send + 'static,
    ) -> anyhow::Result<()> {
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        std::thread::Builder::new().name("uygulama-sesi".into()).spawn(move || unsafe {
            let client = match open(target) {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            // Destructure the returned tuple into three variables.
            let (client, capture, event) = client;
            let _ = ready_tx.send(Ok(()));
            let channels = CHANNELS as usize;
            while !stop.load(Ordering::Relaxed) {
                // Sleep until Windows signals that audio is ready (or 100 ms pass,
                // so we notice `stop` even when the app is silent).
                WaitForSingleObject(event, 100);
                // Drain all packets that are waiting.
                loop {
                    let Ok(n) = capture.GetNextPacketSize() else { break };
                    if n == 0 {
                        break;
                    }
                    // Raw pointer that Windows fills in with the packet's address.
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    if capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                        break;
                    }
                    let len = frames as usize * channels;
                    if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                        // Windows says "this packet is silence": send zeros.
                        on_data(&vec![0f32; len]);
                    } else {
                        // View the raw bytes as a slice of f32 samples (no copy).
                        // Valid until `ReleaseBuffer` below.
                        on_data(std::slice::from_raw_parts(data as *const f32, len));
                    }
                    let _ = capture.ReleaseBuffer(frames);
                }
            }
            let _ = client.Stop();
            let _ = CloseHandle(event);
            log::info!("uygulama sesi yakalama durdu");
        })?;
        // `recv_timeout` gives Result<Result<()>, _>. `.map_err(...)?` handles the
        // outer one (timeout); the inner Result is returned as-is as the
        // function's result.
        ready_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("ses yakalama yanıt vermedi"))?
    }

    /// Opens a process-loopback audio client for `target`.
    /// Returns the audio client, the capture interface and the "data ready" event.
    unsafe fn open(
        target: super::Target,
    ) -> anyhow::Result<(IAudioClient, IAudioCaptureClient, windows::Win32::Foundation::HANDLE)> {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        // App: "capture the audio of this process and its children".
        // AllButUs: "capture everything *except* our own process (and children)".
        let (pid, mode) = match target {
            super::Target::App(pid) => (pid, PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE),
            super::Target::AllButUs => (std::process::id(), PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE),
        };
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: mode,
                },
            },
        };
        // The parameters must be passed inside a PROPVARIANT of type VT_BLOB (a
        // generic "blob of bytes" container). Because the blob points at `params`
        // on the stack, PropVariantClear must NOT be called on it (it would try to
        // free stack memory): `ManuallyDrop` prevents the automatic cleanup.
        const VT_BLOB: u16 = 65;
        // `zeroed()` creates a value with all bytes 0 (unsafe: only valid for
        // plain C-like structs such as this one).
        let mut raw: windows::core::imp::PROPVARIANT = std::mem::zeroed();
        raw.Anonymous.Anonymous.vt = VT_BLOB;
        raw.Anonymous.Anonymous.Anonymous.blob = windows::core::imp::BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: &params as *const _ as *mut u8,
        };
        let pv = std::mem::ManuallyDrop::new(windows::core::PROPVARIANT::from_raw(raw));
        let (tx, rx) = mpsc::channel();
        // `.into()` wraps our Rust struct into a real COM interface pointer.
        let handler: IActivateAudioInterfaceCompletionHandler = Handler(Mutex::new(Some(tx))).into();
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv as *const _), &handler)
            .context("ses yakalama başlatılamadı (Windows 10 2004 veya üstü gerekir)")?;
        // Wait for the completion handler's signal.
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("ses yakalama etkinleştirilemedi"))?;
        let mut hr = HRESULT(0);
        let mut unk: Option<IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok().context("ses yakalama reddedildi")?;
        let client: IAudioClient = unk.ok_or_else(|| anyhow!("ses istemcisi yok"))?.cast()?;

        // Ask for 32-bit float samples. WAVE_FORMAT_IEEE_FLOAT is defined here so we
        // don't need to enable the extra Win32_Media_Multimedia crate feature.
        const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_IEEE_FLOAT,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * CHANNELS * 4,
            nBlockAlign: (CHANNELS * 4) as u16,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            // LOOPBACK: capture what's being played; EVENTCALLBACK: signal an event
            // when data is ready; AUTOCONVERTPCM: let Windows convert to our format.
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            200_000, // 20 ms (in units of 100 ns)
            0,
            &format,
            None,
        )?;
        let event = CreateEventW(None, BOOL(0), BOOL(0), None)?;
        client.SetEventHandle(event)?;
        // `GetService()` infers which interface to return from the variable's type.
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        log::info!("uygulama sesi yakalanıyor (pid {pid})");
        Ok((client, capture, event))
    }
}

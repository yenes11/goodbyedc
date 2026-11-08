//! Bir uygulamanın (ör. Spotify) sesini yakalama.
//!
//! Windows 10 2004 ve sonrası, tek bir sürecin (ve alt süreçlerinin) sesini
//! "process loopback" ile yakalamaya izin verir. Böylece yalnızca seçilen
//! uygulamanın sesi gider; arkadaşların sesi ve bildirimler karışmaz.
//! Diğer sistemlerde şimdilik desteklenmiyor.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Ses çalan bir uygulama.
#[derive(Clone, Debug)]
pub struct AppAudio {
    pub pid: u32,
    pub name: String,
}

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u32 = 2;

pub fn supported() -> bool {
    cfg!(windows)
}

/// Yakalamayı durdurmak için tutamaç (bırakılınca durur).
pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(windows)]
pub use imp::list;

#[cfg(not(windows))]
pub fn list() -> Vec<AppAudio> {
    Vec::new()
}

/// `pid` sürecinin sesini yakalar; 48 kHz stereo 16 bit örnekleri `on_data`'ya verir.
pub fn capture(pid: u32, on_data: impl FnMut(&[i16]) + Send + 'static) -> anyhow::Result<Capture> {
    let stop = Arc::new(AtomicBool::new(false));
    #[cfg(windows)]
    {
        imp::start(pid, stop.clone(), on_data)?;
        Ok(Capture { stop })
    }
    #[cfg(not(windows))]
    {
        let _ = (pid, on_data, stop);
        anyhow::bail!("Uygulama sesi paylaşımı şimdilik yalnızca Windows'ta çalışıyor")
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use anyhow::{anyhow, Context};
    use windows::core::{implement, Interface, IUnknown, HRESULT, PWSTR};
    use windows::Win32::Foundation::{CloseHandle, BOOL};
    use windows::Win32::System::Threading::CreateEventW;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IAgileObject, IAgileObject_Impl, CLSCTX_ALL, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    use super::{AppAudio, CHANNELS, SAMPLE_RATE};

    /// Varsayılan hoparlörde ses oturumu olan uygulamalar.
    pub fn list() -> Vec<AppAudio> {
        std::thread::spawn(|| unsafe { list_inner().unwrap_or_default() }).join().unwrap_or_default()
    }

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
                if s2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK {
                    continue;
                }
                let Ok(pid) = s2.GetProcessId() else { continue };
                if pid == 0 || pid == me || out.iter().any(|a| a.pid == pid) {
                    continue;
                }
                let Some(name) = process_name(pid) else { continue };
                if out.iter().any(|a| a.name == name) {
                    continue;
                }
                out.push(AppAudio { pid, name });
            }
        }
        out.sort_by_key(|a| a.name.to_lowercase());
        Ok(out)
    }

    unsafe fn process_name(pid: u32) -> Option<String> {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        let stem = std::path::Path::new(&path).file_stem()?.to_string_lossy().to_string();
        let mut c = stem.chars();
        Some(match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => stem,
        })
    }

    #[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
    struct Handler(Mutex<Option<mpsc::Sender<()>>>);

    impl IActivateAudioInterfaceCompletionHandler_Impl for Handler {
        fn ActivateCompleted(
            &self,
            _op: Option<&IActivateAudioInterfaceAsyncOperation>,
        ) -> windows::core::Result<()> {
            if let Some(tx) = self.0.lock().unwrap().take() {
                let _ = tx.send(());
            }
            Ok(())
        }
    }

    impl IAgileObject_Impl for Handler {}

    pub fn start(
        pid: u32,
        stop: Arc<AtomicBool>,
        mut on_data: impl FnMut(&[i16]) + Send + 'static,
    ) -> anyhow::Result<()> {
        let (ready_tx, ready_rx) = mpsc::channel::<anyhow::Result<()>>();
        std::thread::Builder::new().name("uygulama-sesi".into()).spawn(move || unsafe {
            let client = match open(pid) {
                Ok(c) => c,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let (client, capture, event) = client;
            let _ = ready_tx.send(Ok(()));
            let channels = CHANNELS as usize;
            while !stop.load(Ordering::Relaxed) {
                WaitForSingleObject(event, 100);
                loop {
                    let Ok(n) = capture.GetNextPacketSize() else { break };
                    if n == 0 {
                        break;
                    }
                    let mut data: *mut u8 = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    if capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                        break;
                    }
                    let len = frames as usize * channels;
                    if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                        on_data(&vec![0i16; len]);
                    } else {
                        on_data(std::slice::from_raw_parts(data as *const i16, len));
                    }
                    let _ = capture.ReleaseBuffer(frames);
                }
            }
            let _ = client.Stop();
            let _ = CloseHandle(event);
            log::info!("uygulama sesi yakalama durdu");
        })?;
        ready_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("ses yakalama yanıt vermedi"))?
    }

    unsafe fn open(
        pid: u32,
    ) -> anyhow::Result<(IAudioClient, IAudioCaptureClient, windows::Win32::Foundation::HANDLE)> {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let params = AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        };
        // VT_BLOB türünde PROPVARIANT. Blob yığındaki `params`'ı gösterdiği için
        // PropVariantClear çağrılmamalı: ManuallyDrop ile bırakılmaz.
        const VT_BLOB: u16 = 65;
        let mut raw: windows::core::imp::PROPVARIANT = std::mem::zeroed();
        raw.Anonymous.Anonymous.vt = VT_BLOB;
        raw.Anonymous.Anonymous.Anonymous.blob = windows::core::imp::BLOB {
            cbSize: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: &params as *const _ as *mut u8,
        };
        let pv = std::mem::ManuallyDrop::new(windows::core::PROPVARIANT::from_raw(raw));
        let (tx, rx) = mpsc::channel();
        let handler: IActivateAudioInterfaceCompletionHandler = Handler(Mutex::new(Some(tx))).into();
        let op = ActivateAudioInterfaceAsync(VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK, &IAudioClient::IID, Some(&*pv as *const _), &handler)
            .context("ses yakalama başlatılamadı (Windows 10 2004 veya üstü gerekir)")?;
        rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("ses yakalama etkinleştirilemedi"))?;
        let mut hr = HRESULT(0);
        let mut unk: Option<IUnknown> = None;
        op.GetActivateResult(&mut hr, &mut unk)?;
        hr.ok().context("ses yakalama reddedildi")?;
        let client: IAudioClient = unk.ok_or_else(|| anyhow!("ses istemcisi yok"))?.cast()?;

        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: CHANNELS as u16,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * CHANNELS * 2,
            nBlockAlign: (CHANNELS * 2) as u16,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            200_000, // 20 ms (100 ns birimleriyle)
            0,
            &format,
            None,
        )?;
        let event = CreateEventW(None, BOOL(0), BOOL(0), None)?;
        client.SetEventHandle(event)?;
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        log::info!("uygulama sesi yakalanıyor (pid {pid})");
        Ok((client, capture, event))
    }
}

//! Windows: keep other apps' audio at full volume while we're in a voice channel.
//!
//! When an app opens the microphone for "communications", Windows by default
//! lowers all other sounds by 80% ("ducking"; Sound settings > Communications).
//! Like Discord, we opt our own audio sessions out of triggering that.
//!
//! The Windows audio APIs are COM interfaces, called through the `windows` crate.
//! Every COM call is `unsafe` in Rust because the compiler can't verify it.

// Compiled only on Windows. On other systems the empty version at the bottom is
// used, so callers can call `ducking::disable()` everywhere without `#[cfg]`.
#[cfg(windows)]
pub fn disable() {
    // `use` inside a function limits these imports to this function.
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eCapture, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    // Use COM on a fresh thread so we don't change the COM mode of tokio's worker
    // threads. `thread::spawn(closure)` starts a thread; `.join()` waits for it and
    // returns what the closure returned (or an error if the thread panicked).
    let res = std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        // `?` returns the error from the closure if creating the enumerator fails.
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mut done = 0;
        // Both speakers (render) and microphones (capture).
        for flow in [eRender, eCapture] {
            // `let Ok(x) = ... else { continue };` = skip this item on error.
            let Ok(devices) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { continue };
            for i in 0..devices.GetCount().unwrap_or(0) {
                let Ok(device) = devices.Item(i) else { continue };
                // `Activate::<T>` uses "turbofish" syntax to say which interface we want.
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                // This process's default audio session on that device (WebRTC uses it too).
                let Ok(control) = manager.GetAudioSessionControl(None, 0) else { continue };
                // `cast` asks the COM object for another interface it implements.
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                // `true` = "opt out": this session won't duck other sounds.
                if control2.SetDuckingPreference(true).is_ok() {
                    done += 1;
                }
            }
        }
        // Tell the compiler the closure's error type explicitly (it can't infer it).
        Ok::<_, windows::core::Error>(done)
    })
    .join();
    // `match` checks every possible shape of the result:
    //   Ok(Ok(n))  -> thread ran and COM calls worked
    //   Ok(Err(e)) -> thread ran but a COM call failed
    //   Err(_)     -> the thread panicked
    match res {
        Ok(Ok(n)) => log::info!("ses kısma kapatıldı ({n} oturum)"),
        Ok(Err(e)) => log::warn!("ses kısma kapatılamadı: {e}"),
        Err(_) => log::warn!("ses kısma kapatılamadı"),
    }
}

/// Non-Windows: nothing to do.
#[cfg(not(windows))]
pub fn disable() {}

/// Windows: the id of the default *console* speaker device (what Windows calls
/// "Default Device", as opposed to "Default Communication Device").
///
/// Why this matters: WebRTC plays incoming voices on the default
/// *communications* device. Windows treats any stream on that device as a call
/// and then lowers every other app (Spotify, games, ...). `SetDuckingPreference`
/// in `disable()` only stops *our* sound from being lowered, not others. Playing
/// on the console default device instead avoids the trigger altogether.
/// voice.rs passes this id to WebRTC (`set_playout_device_by_guid`); WebRTC's
/// Windows audio code uses the same endpoint id strings as device "guids".
#[cfg(windows)]
pub fn default_render_id() -> Option<String> {
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_ALL, COINIT_MULTITHREADED};

    // Same fresh-thread trick as in `disable` (keeps COM setup off tokio's threads).
    std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        // `GetId` returns a wide (UTF-16) string allocated by COM; we copy it into
        // a Rust `String` and then must free the original with `CoTaskMemFree`.
        let pwstr = device.GetId().ok()?;
        let id = pwstr.to_string().ok();
        CoTaskMemFree(Some(pwstr.0 as *const _));
        id
    })
    .join()
    .ok()
    .flatten()
}

/// Non-Windows: no communications/console distinction; keep WebRTC's default.
#[cfg(not(windows))]
pub fn default_render_id() -> Option<String> {
    None
}

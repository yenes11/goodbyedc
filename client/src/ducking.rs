//! Windows: ses kanalına girince diğer uygulamaların sesi kısılmasın.
//!
//! Windows, bir uygulama mikrofonu iletişim için açtığında diğer sesleri
//! varsayılan olarak %80 kısar ("iletişim etkinliği"). Discord gibi bu
//! uygulama da kendi ses oturumlarını bu davranıştan çıkarır.

#[cfg(windows)]
pub fn disable() {
    use windows::core::Interface;
    use windows::Win32::Media::Audio::{
        eCapture, eRender, IAudioSessionControl2, IAudioSessionManager2, IMMDeviceEnumerator, MMDeviceEnumerator,
        DEVICE_STATE_ACTIVE,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};

    // COM'u ayrı bir iş parçacığında kullan (tokio iş parçacıklarını etkilemesin).
    let res = std::thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mut done = 0;
        for flow in [eRender, eCapture] {
            let Ok(devices) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) else { continue };
            for i in 0..devices.GetCount().unwrap_or(0) {
                let Ok(device) = devices.Item(i) else { continue };
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                // Bu sürecin o aygıttaki varsayılan oturumu (WebRTC de bunu kullanır).
                let Ok(control) = manager.GetAudioSessionControl(None, 0) else { continue };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else { continue };
                if control2.SetDuckingPreference(true).is_ok() {
                    done += 1;
                }
            }
        }
        Ok::<_, windows::core::Error>(done)
    })
    .join();
    match res {
        Ok(Ok(n)) => log::info!("ses kısma kapatıldı ({n} oturum)"),
        Ok(Err(e)) => log::warn!("ses kısma kapatılamadı: {e}"),
        Err(_) => log::warn!("ses kısma kapatılamadı"),
    }
}

#[cfg(not(windows))]
pub fn disable() {}

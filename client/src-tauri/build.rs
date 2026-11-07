fn main() {
    // libwebrtc'nin Objective-C kategorileri (macOS) uygulama binary'sine
    // bağlanmazsa açılışta "unrecognized selector" hatasıyla çöker.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg=-ObjC");
    }
    tauri_build::build()
}

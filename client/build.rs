fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // libwebrtc'nin Objective-C kategorileri için gerekli (yoksa açılışta çöker).
    if os == "macos" {
        println!("cargo:rustc-link-arg=-ObjC");
    }
    // Windows: exe simgesi ve sürüm bilgisi.
    #[cfg(windows)]
    if os == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico");
        res.set("ProductName", "goodbyedc");
        res.set("FileDescription", "goodbyedc");
        res.compile().expect("Windows kaynak dosyası derlenemedi");
    }
    println!("cargo:rerun-if-changed=icons/icon.ico");
}

// Build script.
//
// Cargo compiles and runs `build.rs` *before* compiling the app itself. It is a
// normal Rust program whose `println!("cargo:...")` lines are instructions to
// Cargo (extra linker flags, when to re-run, etc.). We use it for two
// platform-specific things.
fn main() {
    // Cargo tells the build script which OS we are compiling *for* through an
    // environment variable. (We can't use `cfg!(target_os = ...)` here because the
    // build script itself always runs on the machine doing the build.)
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    // macOS: libwebrtc ships Objective-C "categories" (methods added to existing
    // classes). The `-ObjC` linker flag forces the linker to keep them; without it
    // the app crashes at startup when WebRTC calls a method that was dropped.
    if os == "macos" {
        println!("cargo:rustc-link-arg=-ObjC");
    }

    // Windows: embed the app icon and version info into the .exe file.
    // `#[cfg(windows)]` means this block only exists when the *build machine* is
    // Windows (the `winresource` crate is only a dependency there).
    #[cfg(windows)]
    if os == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico");
        res.set("ProductName", "goodbyedc");
        res.set("FileDescription", "goodbyedc");
        // `.expect(msg)` turns an error into a crash with that message. In a build
        // script that is fine: the build simply fails and shows the message.
        res.compile().expect("Windows kaynak dosyası derlenemedi");
    }

    // Only re-run this script when the icon changes (otherwise Cargo would re-run
    // it on every change to any file in the package).
    println!("cargo:rerun-if-changed=icons/icon.ico");
}

//! Puts `assets/icon.ico` on the Windows executables, so Explorer and the taskbar show the app's
//! icon before it opens a window.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The icon is a nicety: a machine without the resource compiler still builds and runs.
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("assets/icon.ico");
    if let Err(error) = resource.compile() {
        println!("cargo:warning=Could not embed assets/icon.ico: {error}");
    }
}

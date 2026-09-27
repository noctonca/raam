// The shared gl.rs declares the GL entry points without a #[link]; the
// desktop host resolves them from the system's GL. The EGL declarations
// in it are never called here, so they need no library.
fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    match os.as_str() {
        "macos" => println!("cargo:rustc-link-lib=framework=OpenGL"),
        "linux" => println!("cargo:rustc-link-lib=GL"),
        _ => {}
    }
}

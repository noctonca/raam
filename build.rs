// The shared gl.rs declares the GL entry points without a #[link]; the
// desktop host resolves them from the system's GL, or its GLES2 with the
// `gles` feature. The EGL declarations in it are never called here, so
// they need no library.
fn main() {
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let gles = std::env::var_os("CARGO_FEATURE_GLES").is_some();
    match os.as_str() {
        "macos" => println!("cargo:rustc-link-lib=framework=OpenGL"),
        "linux" if gles => println!("cargo:rustc-link-lib=GLESv2"),
        "linux" => println!("cargo:rustc-link-lib=GL"),
        _ => {}
    }
}

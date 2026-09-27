//! Links the Android system libraries the host talks to directly: EGL and
//! GLESv2 (the render stack) and OpenSLES (audio). No bindgen (the OpenSL
//! bindings are checked in, sles_sys.rs) and no build-time secrets: the
//! server and key are entered in settings, never baked in.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    println!("cargo:rustc-link-lib=dylib=EGL");
    println!("cargo:rustc-link-lib=dylib=GLESv2");
    println!("cargo:rustc-link-lib=OpenSLES");
}

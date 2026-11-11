fn main() {
    // sherpa-onnx (feature `sherpa`) links shared libraries (libsherpa-onnx-c-api.so,
    // libonnxruntime.so) that its build copies next to the binary. Windows finds DLLs there by
    // itself; on Linux the loader needs a RUNPATH of `$ORIGIN` (the executable's directory).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
    tauri_build::build()
}

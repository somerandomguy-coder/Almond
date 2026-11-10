//! Windows + `sherpa` feature: sherpa-onnx-sys copies its runtime DLLs (sherpa-onnx-c-api.dll,
//! onnxruntime.dll, ...) into `target/<profile>/` only. Test binaries live in `deps/` and
//! examples in `examples/`, where Windows would otherwise pick up the *system* onnxruntime.dll
//! (C:\Windows\System32, ORT 1.17) and crash with "requested API version [N] is not available".
//! Copy the DLLs next to those binaries too. (sherpa-onnx-sys has `links = "sherpa-onnx"`, so its
//! build script — and the copy into the profile dir — has run before this one.)

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !windows || std::env::var_os("CARGO_FEATURE_SHERPA").is_none() {
        return;
    }
    let Some(out) = std::env::var_os("OUT_DIR") else {
        return;
    };
    // OUT_DIR = <target>/<profile>/build/almond-stt-<hash>/out
    let Some(profile) = std::path::Path::new(&out).ancestors().nth(3) else {
        return;
    };
    let dlls = std::fs::read_dir(profile)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            n.ends_with(".dll") && (n.starts_with("sherpa-onnx") || n.starts_with("onnxruntime"))
        })
        .collect::<Vec<_>>();
    for sub in ["deps", "examples"] {
        let dir = profile.join(sub);
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        for d in &dlls {
            let dst = dir.join(d.file_name().unwrap());
            let stale = match (d.metadata(), dst.metadata()) {
                (Ok(a), Ok(b)) => a.len() != b.len(),
                _ => true,
            };
            if stale {
                let _ = std::fs::copy(d, &dst);
            }
        }
    }
}

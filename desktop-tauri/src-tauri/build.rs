fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    if !target.is_empty() {
        let binaries_dir = std::path::Path::new("binaries");
        let _ = std::fs::create_dir_all(binaries_dir);
        let ext = if target.contains("windows") {
            ".exe"
        } else {
            ""
        };
        for name in &["mkv-remux", "mkv-extract"] {
            let sidecar = binaries_dir.join(format!("{name}-{target}{ext}"));
            if !sidecar.exists() {
                let _ = std::fs::write(&sidecar, b"");
            }
        }
    }
    tauri_build::build();
}

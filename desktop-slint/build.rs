//! The Windows exe's icon and version resources (the Tauri app's icon; the version, product name
//! and description from Cargo), compiled with tauri-winres, which the Tauri host's build already
//! uses. Nothing on other targets.

fn main() {
    println!("cargo:rerun-if-changed=../desktop-tauri/src-tauri/icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = tauri_winres::WindowsResource::new();
    resource
        .set_icon("../desktop-tauri/src-tauri/icons/icon.ico")
        .set("ProductName", "MKV Anime Batch")
        .set("FileDescription", "MKV Anime Batch");
    resource
        .compile()
        .expect("compile the Windows exe's resources");
}

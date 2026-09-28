use tauri_build::WindowsAttributes;

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(WindowsAttributes::new_without_app_manifest()),
    )
    .expect("failed to run tauri-build");

    // tauri-build's resource-embedded manifest only reaches the app binary. Embed the
    // Common-Controls v6 manifest through the linker instead so test binaries, which
    // link the same comctl32 v6 imports, load too.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
        let manifest_path = std::path::Path::new(&out_dir).join("windows-app-manifest.xml");
        std::fs::write(
            &manifest_path,
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
"#,
        )
        .expect("failed to write windows-app-manifest.xml");

        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
            manifest_path.display()
        );
    }
}

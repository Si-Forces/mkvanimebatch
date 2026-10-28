fn main() {
    // Pin the std-widgets style, so the look does not follow the build host's platform default.
    // Debug builds keep the element info the host's headless tests search (`ElementHandle`,
    // `find_by_element_type_name`); release builds drop it, so the UI tests need a debug build:
    // under `cargo test --release` they fail.
    let config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_debug_info(std::env::var("PROFILE").as_deref() == Ok("debug"));
    slint_build::compile_with_config("app.slint", config).expect("compile app.slint");
}

fn main() {
    // Pin the std-widgets style, so the look does not follow the build host's platform default.
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("app.slint", config).expect("compile app.slint");
}

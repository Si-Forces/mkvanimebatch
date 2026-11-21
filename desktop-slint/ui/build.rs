fn main() {
    // Pin the std-widgets style, so the look does not follow the build host's platform default.
    // Debug builds keep the element info the host's headless tests search (`ElementHandle`,
    // `find_by_element_type_name`); release builds drop it, so the UI tests need a debug build:
    // under `cargo test --release` they fail.
    // Edition 1.19's language rules
    // (https://releases.slint.dev/1.19.0/docs/slint/guide/language/concepts/editions/#behavior-changes):
    // - `if`/`for` children size a non-layout parent (#407), and a wrapped Text with its own
    //   `width` in one is measured at that width, not the layout's (#14705);
    // - a property set by only some states keeps its default in the others (#8852);
    // - a binding loop inside a longer one is an error too, so a later edit can hit a loop error
    //   the 1.18 rules did not report.
    // Only this build sets it (no slint-project.json): slint-lsp, slint-viewer and the preview
    // lay out with the 1.18 rules unless given `--edition 1.19`.
    let config = slint_build::CompilerConfiguration::new()
        .with_edition((1, 19))
        .with_style("fluent".into())
        .with_debug_info(std::env::var("PROFILE").as_deref() == Ok("debug"));
    slint_build::compile_with_config("app.slint", config).expect("compile app.slint");
}

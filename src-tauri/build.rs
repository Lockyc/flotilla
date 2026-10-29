fn main() {
    // Build stamp (BUILD_GIT_SHA/BUILD_DATE → About) + materialize the shared release scripts into
    // repo-root scripts/ (git-ignored; scripts/tooling.env supplies flotilla's params), both from
    // the pinned shell-core crate.
    shell_core::build_stamp();
    shell_core::materialize_scripts(std::path::Path::new("../scripts"))
        .expect("materialize shell-core scripts");

    // Materialize the shared chrome into frontendDist (../src) so generate_context! embeds it.
    // The generated files are git-ignored — reproducible from the pinned chrome-core rev.
    let src = std::path::Path::new("../src");
    std::fs::write(src.join("chrome-core.css"), chrome_core::SIDEBAR_CSS)
        .expect("write chrome-core.css");
    std::fs::write(src.join("chrome-core.js"), chrome_core::SIDEBAR_JS)
        .expect("write chrome-core.js");

    tauri_build::build()
}

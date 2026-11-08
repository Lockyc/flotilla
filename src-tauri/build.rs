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

    bake_google_client();

    tauri_build::build()
}

/// Bakes the Google OAuth client into the binary: `FLOTILLA_GOOGLE_CLIENT_ID` / `_SECRET` from the
/// environment, else from `$XDG_CONFIG_HOME/flotilla/google-client.env` (`KEY=value` lines). Either
/// stays empty when absent, and `google::client()` is then `None`. The values never reach build output.
fn bake_google_client() {
    const KEYS: [&str; 2] = ["FLOTILLA_GOOGLE_CLIENT_ID", "FLOTILLA_GOOGLE_CLIENT_SECRET"];
    for var in KEYS.iter().copied().chain(["XDG_CONFIG_HOME", "HOME"]) {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")));
    let file = base.map(|b| b.join("flotilla").join("google-client.env"));
    // A missing path is always stale to Cargo, so watch the nearest existing ancestor until the
    // file exists; creating it then still triggers a rerun.
    if let Some(watch) = file
        .as_deref()
        .and_then(|f| f.ancestors().find(|p| p.exists()))
    {
        println!("cargo:rerun-if-changed={}", watch.display());
    }
    let contents = file
        .and_then(|f| std::fs::read_to_string(f).ok())
        .unwrap_or_default();
    for key in KEYS {
        let value = std::env::var(key)
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| {
                contents.lines().find_map(|line| {
                    let line = line.trim();
                    let (k, v) = line
                        .strip_prefix("export ")
                        .unwrap_or(line)
                        .split_once('=')?;
                    let v = v.trim();
                    let v = ['"', '\'']
                        .iter()
                        .find_map(|q| v.strip_prefix(*q)?.strip_suffix(*q))
                        .unwrap_or(v);
                    (k.trim() == key).then(|| v.to_string())
                })
            })
            .unwrap_or_default();
        println!("cargo:rustc-env={key}={value}");
    }
}

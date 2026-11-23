fn main() {
    // The Client window's permissions (`capabilities/client.json`) exist only in builds
    // with the `client` feature; Phase LAN gets the main window's capability alone.
    println!("cargo:rerun-if-changed=capabilities");
    let pattern = if std::env::var_os("CARGO_FEATURE_CLIENT").is_some() {
        "./capabilities/*.json"
    } else {
        "./capabilities/default.json"
    };
    tauri_build::try_build(tauri_build::Attributes::new().capabilities_path_pattern(pattern))
        .expect("failed to run the Tauri build script");
}

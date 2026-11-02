fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "updater_status",
            "updater_install",
            "updater_skip",
        ]),
    ))
    .expect("desktop build failed");
}

//! Headless release-asset validation. Never creates a window or reads user data.
#[tokio::test]
#[ignore = "requires production assets and CADPAD_FRONTEND_CHECK_DIR; candidate builder runs this explicitly"]
async fn release_assets_and_real_startup_reply_are_complete() {
    use std::path::{Component, Path, PathBuf};
    use tauri::utils::assets::AssetKey;
    let root = PathBuf::from(
        std::env::var_os("CADPAD_FRONTEND_CHECK_DIR").expect("Set an owned empty output directory"),
    );
    assert!(root.is_dir() && std::fs::read_dir(&root).unwrap().next().is_none());
    let context: tauri::Context<tauri::Wry> = tauri::generate_context!(test = true);
    let html = context
        .assets
        .get(&AssetKey::from("index.html"))
        .expect("Release frontend is not embedded");
    let html = std::str::from_utf8(&html).unwrap();
    assert!(
        html.contains("Opening your workspace"),
        "Missing static startup shell"
    );
    assert!(
        html.contains("/startup.js"),
        "Missing pre-module error handler"
    );
    assert!(
        html.find("/startup.js").unwrap()
            < html.find("type=\"module\"").expect("Missing module entry"),
        "Bootstrap must install error handlers before module loading starts"
    );
    let keys = context
        .assets
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    assert!(
        keys.len() > 3,
        "Expected JavaScript, CSS and bootstrap assets"
    );
    for key in &keys {
        let relative = Path::new(key.trim_start_matches('/'));
        assert!(
            relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        );
        let data = context.assets.get(&AssetKey::from(key.as_str())).unwrap();
        let destination = root.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(destination, data).unwrap();
    }
    let data_root = root.join("probe-data");
    let core = cadpad_core::Core::open(
        data_root.clone(),
        cadpad_core::worker::WorkerConfig::bundled("unused".into()),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("local-user").await.unwrap();
    core.focus_workspace(&ws).await.unwrap();
    let project = core.get_project(&ws).await.unwrap();
    std::fs::write(
        root.join("startup-project.json"),
        serde_json::to_vec(&project).unwrap(),
    )
    .unwrap();
    drop(core);
    std::fs::remove_dir_all(data_root).unwrap();
    eprintln!(
        "Validated and extracted {} production assets and native startup reply",
        keys.len()
    );
}

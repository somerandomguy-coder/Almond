//! Opens a real `Core` (SQLite store, localhost-only sync) in a temp dir and calls it through the
//! router, as the Tauri shell does.

mod common;

use common::open_core;
use serde_json::{Value, json};

#[tokio::test(flavor = "multi_thread")]
async fn settings_and_app_info() {
    let (api, dir) = open_core("settings").await;

    assert_eq!(
        api.call("settings.get", json!({ "key": "ui.language" }))
            .await
            .unwrap(),
        Value::Null
    );
    api.call(
        "settings.set",
        json!({ "key": "ui.language", "value": "vi" }),
    )
    .await
    .unwrap();
    assert_eq!(
        api.call("settings.get", json!({ "key": "ui.language" }))
            .await
            .unwrap(),
        json!("vi")
    );

    let info = api.call("app.info", Value::Null).await.unwrap();
    assert_eq!(info["name"], "Almond");
    assert_eq!(info["device_name"], "Test device");

    let err = api.call("no.such", Value::Null).await.unwrap_err();
    assert_eq!(err.code, -32601);
    let err = api
        .call("settings.get", json!({ "nope": 1 }))
        .await
        .unwrap_err();
    assert_eq!(err.code, -32602);

    api.core.shutdown().await;
    drop(api);
    let _ = std::fs::remove_dir_all(dir);
}

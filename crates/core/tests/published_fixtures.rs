#[path = "support/published_fixture.rs"]
mod published_fixture;

use cadpad_core::worker::WorkerConfig;
use serde_json::{Value, json};
use std::path::PathBuf;

#[tokio::test]
async fn published_fixture_copies_are_independent_and_native_recovery_uses_the_copy() {
    let temp = tempfile::tempdir().unwrap();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let config = WorkerConfig::development(&repo).uncontained_for_trusted_tests();
    let source = r#"
import sys
from pathlib import Path
import cadquery as cq
cache = Path(sys.argv[sys.argv.index('--cache') + 1])
with (cache / 'fixture-builds').open('a') as stream:
    stream.write('built\n')
show(cq.Workplane().box(2,3,4), name='box')
"#;
    let a = temp.path().join("a");
    let b = temp.path().join("b");
    let (first, workspace, revision) =
        published_fixture::open_shared(a.clone(), config.clone(), source, Value::Null).await;
    let (second, second_ws, second_rev) =
        published_fixture::open_shared(b.clone(), config.clone(), source, Value::Null).await;
    assert_eq!((&workspace, &revision), (&second_ws, &second_rev));
    assert!(
        !b.join("runs").exists(),
        "A copied fixture must not execute source during setup"
    );
    let project = second.get_project(&workspace).await.unwrap();
    assert!(project["display_error"].is_null());
    let handle = project["model"]["parts"][0]["mesh"]["buffers"]["vertices"]["handle"]
        .as_str()
        .unwrap();
    let relative = PathBuf::from("artifacts").join(&workspace).join(&revision);
    let original = std::fs::read(b.join(&relative).join(handle)).unwrap();
    std::fs::write(a.join(&relative).join(handle), b"corrupt first copy").unwrap();
    assert_eq!(
        std::fs::read(b.join(&relative).join(handle)).unwrap(),
        original
    );
    let inspected = second
        .query(
            &workspace,
            &revision,
            "inspect",
            json!({"entities":[{"revision":revision,"instance_id":"box","entity_id":"part:0"}]}),
        )
        .await
        .unwrap();
    assert!((inspected["entities"][0]["volume"].as_f64().unwrap() - 24.).abs() < 1e-9);
    assert_eq!(
        std::fs::read_to_string(b.join(&relative).join("fixture-builds"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert_eq!(
        std::fs::read_to_string(a.join(&relative).join("fixture-builds"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let c = temp.path().join("c");
    let (third, _, _) =
        published_fixture::open_shared(c.clone(), config, source, Value::Null).await;
    assert_eq!(
        std::fs::read(c.join(relative).join(handle)).unwrap(),
        original
    );
    assert!(third.get_project(&workspace).await.unwrap()["display_error"].is_null());
    drop(first);
}

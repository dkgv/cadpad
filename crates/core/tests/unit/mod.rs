//! Core contract tests whose precondition is published metadata, not a CAD build.
//! A missing worker executable makes any accidental native dependency fail.
//! Native geometry, coverage, pose inspection and exports remain in integration
//! tests (motion, dynamics_integration, geometry_contracts and the agent loop).
mod dynamics;
mod views;

use crate::{Core, worker::WorkerConfig};
use serde_json::{Value, json};
use std::path::Path;

async fn published(root: &Path, mut model: Value, mechanism: Value) -> (Core, String, String) {
    let core = Core::open(
        root.into(),
        WorkerConfig::bundled(root.join("no-native-worker")),
    )
    .await
    .unwrap();
    let ws = core.create_workspace("user").await.unwrap();
    let rev = {
        let mut all = core.workspaces.lock().await;
        let workspace = all.get_mut(&ws).unwrap();
        let rev = workspace.stored.current.clone();
        let revision = workspace.stored.revisions.get_mut(&rev).unwrap();
        revision.inputs.mechanism = mechanism;
        model["revision"] = json!(rev);
        revision.model = model;
        core.persist(&workspace.stored).await.unwrap();
        rev
    };
    assert!(!root.join("runs").exists());
    (core, ws, rev)
}

async fn plate(root: &Path) -> (Core, String, String) {
    published(
        root,
        json!({"parts":[{
            "instance_id":"plate", "position":[0,0,0],
            "bounds":{"min":[-25,-10,-3],"max":[25,10,3],"size":[50,20,6]},
            "volume":6000.0, "face_count":6,"edge_count":12,"features":{}
        }]}),
        json!({}),
    )
    .await
}

async fn rail(root: impl AsRef<Path>) -> (Core, String, String) {
    let mechanism = serde_json::from_str(include_str!(
        "../../../../fixtures/rail_carriage.mechanism.json"
    ))
    .unwrap();
    // Synthetic primitive bodies isolate control/transaction behavior. This
    // metadata is not a substitute for the native BRep-coverage integration test.
    published(root.as_ref(), json!({"parts":[
        {"instance_id":"rail","position":[0,0,0],"bounds":{"min":[-100,-15,-5],"max":[100,15,5]},"volume":60000},
        {"instance_id":"carriage","position":[0,0,13.02],"bounds":{"min":[-20,-20,-8],"max":[20,20,8]},"volume":25600}
    ],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["rail","carriage"]}}), mechanism).await
}

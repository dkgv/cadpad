//! End-to-end requirement runs on real fixture projects (`fixtures/requirements/`):
//! compile, approve (the user-only core API), run on the Python worker, and
//! check status and margins; relink behaviour when sources change between
//! runs; the run queue; the auto-run policy; determinism across pool sizes.
//!
//! Needs the project virtualenv (`.venv`). Run with
//! `cargo test -p cadpad-core --test requirements_runs`.
use cadpad_core::{
    Core, JobState, kinematics, portable,
    requirements::{
        run::RunOptions,
        storage::{ApproveRequest, StoredHashes},
    },
    tools,
    worker::WorkerConfig,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const R1: &str = include_str!("fixtures/requirements/R1.json");
const R2: &str = include_str!("fixtures/requirements/R2.json");
const R4: &str = include_str!("fixtures/requirements/R4.json");
const E3: &str = include_str!("fixtures/requirements/E3.json");
const E5: &str = include_str!("fixtures/requirements/E5.json");
const E6: &str = include_str!("fixtures/requirements/E6.json");
const PROXY1: &str = include_str!("fixtures/requirements/PROXY1.json");

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn config(contained: bool) -> WorkerConfig {
    let config = WorkerConfig::development(&repository());
    if contained {
        config
    } else {
        config.uncontained_for_trusted_tests()
    }
}

struct Project {
    _temp: tempfile::TempDir,
    core: Core,
    workspace: String,
    root: PathBuf,
}

fn copy_fixture(name: &str, to: &Path) {
    let from = repository().join("fixtures/requirements").join(name);
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), to.join(entry.file_name())).unwrap();
    }
}

async fn open(fixture: &str) -> Project {
    open_with(fixture, false).await
}

async fn open_with(fixture: &str, contained: bool) -> Project {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("design");
    std::fs::create_dir(&root).unwrap();
    copy_fixture(fixture, &root);
    portable::initialize_project_browser(&root).unwrap();
    let core = Core::open(temp.path().join("app"), config(contained))
        .await
        .unwrap();
    let project = core.open_project(root.clone(), "user").await.unwrap();
    Project {
        _temp: temp,
        core,
        workspace: project.workspace_id,
        root,
    }
}

fn agent() -> tools::ToolContext {
    tools::ToolContext {
        actor: "agent".into(),
    }
}

impl Project {
    async fn wait_for_job(&self, workspace: &str, job_id: &str) -> cadpad_core::Job {
        tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let current = self.core.get_job(workspace, job_id).await.unwrap();
                if !matches!(current.state, JobState::Queued | JobState::Running) {
                    break current;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("proxy fixture build timed out")
    }

    async fn install_mechanism(&self, workspace: &str, definition: Value) {
        let base = self.core.get_project(workspace).await.unwrap()["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        let draft = self
            .core
            .stage_mechanism(
                workspace,
                &base,
                None,
                kinematics::decode(&definition).unwrap(),
                "user",
            )
            .await
            .unwrap();
        let job = self
            .core
            .build(workspace, &base, &draft.id, "user")
            .await
            .unwrap();
        let finished = self.wait_for_job(workspace, &job.id).await;
        assert_eq!(
            finished.state,
            JobState::Succeeded,
            "{}",
            finished.diagnostics
        );
    }

    async fn call(&self, expected: Option<&str>, command: Value) -> cadpad_core::Result<Value> {
        self.core
            .dispatch(
                &agent(),
                "control_requirements",
                json!({"workspace_id": self.workspace, "expected_catalog": expected, "command": command}),
            )
            .await
    }

    async fn catalog(&self) -> String {
        self.call(None, json!({"action": "list"})).await.unwrap()["catalog"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn list(&self) -> Value {
        self.call(None, json!({"action": "list"})).await.unwrap()
    }

    /// Compile, save the draft and approve it as the user. Returns the id.
    async fn approve(&self, ir: &str) -> String {
        let ir: Value = serde_json::from_str(ir).unwrap();
        let id = ir["id"].as_str().unwrap().to_owned();
        let compiled = self
            .call(None, json!({"action": "compile", "ir": ir}))
            .await
            .unwrap();
        assert_eq!(
            compiled["compiled"]["ok"], true,
            "diagnostics: {}",
            compiled["compiled"]["diagnostics"]
        );
        let token = self.catalog().await;
        let saved = self
            .call(Some(&token), json!({"action": "save", "ir": ir}))
            .await
            .unwrap();
        let hashes: StoredHashes =
            serde_json::from_value(compiled["compiled"]["hashes"].clone()).unwrap();
        let reply = self
            .core
            .approve_requirement(ApproveRequest {
                workspace_id: self.workspace.clone(),
                expected_catalog: saved["catalog"].as_str().map(str::to_owned),
                id: id.clone(),
                hashes,
            })
            .await
            .unwrap();
        let entry = reply
            .requirements
            .iter()
            .find(|r| r.id == id)
            .expect("approved entry");
        assert!(entry.approved.is_some());
        id
    }

    async fn run(&self, id: &str) -> Value {
        let reply = self
            .call(None, json!({"action": "run", "id": id}))
            .await
            .unwrap();
        reply["run"]["result"].clone()
    }

    fn edit(&self, file: &str, from: &str, to: &str) {
        let path = self.root.join(file);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(source.contains(from), "{from:?} not in {file}");
        std::fs::write(path, source.replace(from, to)).unwrap();
    }
}

/// R1 with another sweep step and id (a cheaper or dearer variant).
fn r1_step(id: &str, step_deg: f64) -> String {
    let mut ir: Value = serde_json::from_str(R1).unwrap();
    ir["id"] = json!(id);
    ir["claims"][0]["args"]["over"]["states"]["range"]["step"]["value"] = json!(step_deg);
    ir.to_string()
}

fn margin(result: &Value, claim: usize) -> f64 {
    result["body"]["claims"][claim]["margin"]["value"]
        .as_f64()
        .unwrap()
}

fn approx(a: f64, b: f64) {
    assert!((a - b).abs() < 1e-3, "{a} != {b}");
}

fn finding_labels(result: &Value) -> Vec<String> {
    result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["label"].as_str().unwrap().to_owned())
        .collect()
}

/// Stage `source` as `path`, build it on `view`, and return the build job's
/// P2.23 auto-edit guard.
async fn rebuild_source(p: &Project, view: &str, path: &str, source: &str) -> Value {
    let model = p.core.get_project(view).await.unwrap();
    let revision = model["revision"].as_str().unwrap().to_owned();
    let changes = [(path.to_owned(), source.to_owned())].into_iter().collect();
    let draft = p
        .core
        .update_source(view, &revision, None, changes, "user")
        .await
        .unwrap();
    let job = p
        .core
        .build(view, &revision, &draft.id, "user")
        .await
        .unwrap();
    let job = p.wait_for_job(view, &job.id).await;
    assert_eq!(job.state, JobState::Succeeded, "{}", job.diagnostics);
    job.diagnostics["auto_edit"].clone()
}

fn entry(listed: &Value, id: &str) -> Value {
    listed["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap()
        .clone()
}

// ------------------------------------------------------------------- golden

#[tokio::test]
async fn r1_turntable_rotates_freely() {
    let p = open("turntable").await;
    let id = p.approve(R1).await;
    // Approved but not run: no status yet.
    let listed = p.list().await;
    assert!(listed["requirements"][0]["status"].is_null());
    assert_eq!(listed["requirements"][0]["approval"], "approved");
    assert!(listed["requirements"][0]["draft_hashes"]["plan"].is_string());
    let reply = p
        .call(None, json!({"action": "run", "id": id}))
        .await
        .unwrap();
    assert_eq!(reply["run"]["requirement_id"], id.as_str());
    assert_eq!(reply["run"]["state"], "completed");
    let result = p.run(&id).await;
    assert!(result["finished_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(result["format"], "requirements-run-v1");
    assert_eq!(result["status"], "met", "{result}");
    assert_eq!(result["ran"], true);
    assert_eq!(result["body"]["status"], "met");
    // 0..360 in 5 degree steps; the five moving parts are each paired with the base.
    assert_eq!(result["body"]["primitives"][0]["states"], 73);
    assert_eq!(result["body"]["primitives"][0]["jobs"], 365);
    // Margin is the overlap tolerance itself: nothing overlaps at all.
    approx(margin(&result, 0), 0.001);
    assert!(result["findings"].as_array().unwrap().is_empty());
    assert_eq!(result["relink"]["decision"], "run");
    // The catalog reply carries the stored result and status.
    let after = p.list().await;
    assert_eq!(after["requirements"][0]["status"], "met");
    assert_eq!(
        after["requirements"][0]["last_result"]["plan_hash"],
        result["plan_hash"]
    );
    let primitive = &result["body"]["primitives"][0];
    let replay = p
        .core
        .replay_requirement_state(
            "user",
            &p.workspace,
            &id,
            primitive["id"].as_str().unwrap(),
            primitive["worst"]["state"].as_u64().unwrap() as usize,
        )
        .await
        .unwrap();
    assert!(!replay.view.temporary);
    assert!(matches!(
        replay.mode,
        cadpad_core::requirements::replay::ReplayMode::Rigid
    ));
    assert_eq!(replay.moves[0]["op"], "rotate");
    assert_eq!(replay.moves[0]["axis"], "z");
    assert_eq!(
        replay.moves[0]["parts"],
        json!(["clamp_1", "clamp_2", "rotor", "tower_1", "tower_2"])
    );
}

#[tokio::test]
async fn e3_door_hits_the_frame_past_ninety_degrees() {
    let p = open("cabinet").await;
    let id = p.approve(E3).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "not_met", "{result}");
    // The door overlaps the frame from 92 deg (state 46) to the end at 110.
    approx(margin(&result, 0), -39.4962);
    assert_eq!(
        result["body"]["claims"][0]["worst"]["state_label"],
        "door_angle = 110°"
    );
    let findings = result["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0]["label"], "new");
    assert_eq!(findings[0]["first_state"], 46);
    assert_eq!(findings[0]["last_state"], 55);
    assert_eq!(findings[0]["states"][0]["label"], "door_angle = 92°");
    // The closed door rests on the frame: the allowance holds, so only the
    // ten states from 92 to 110 degrees fail.
    assert_eq!(result["body"]["primitives"][0]["failing_state_count"], 10);
    let primitive = result["body"]["primitives"][0]["id"].as_str().unwrap();
    let shown = p
        .core
        .replay_requirement_state("user", &p.workspace, &id, primitive, 55)
        .await
        .unwrap();
    assert!(shown.view.temporary);
    assert!(matches!(
        shown.mode,
        cadpad_core::requirements::replay::ReplayMode::Parameter
    ));
    let preview = p.core.get_project(&shown.view.workspace).await.unwrap();
    assert_eq!(preview["revision"], shown.view.revision);
    assert_eq!(preview["model"]["parameters"]["door_angle"]["value"], 110.0);
    assert_eq!(
        p.core.workspace_summaries().await.as_array().unwrap().len(),
        1
    );
    assert_eq!(entry(&p.list().await, &id)["last_result"], result);
    p.core
        .close_requirement_replay(&shown.view.workspace)
        .await
        .unwrap();
    assert!(!p.core.has_workspace(&shown.view.workspace).await);
    assert_eq!(p.core.list_workspaces().await.len(), 2);
}

#[tokio::test]
async fn e5_bracket_fits_the_bed_and_is_one_solid() {
    let p = open("bracket").await;
    let id = p.approve(E5).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "met", "{result}");
    // 256 mm bed, 120 mm bracket: 136 mm to spare on the long axis.
    approx(margin(&result, 0), 136.0);
    assert_eq!(result["body"]["claims"][0]["rule"], "fits_bed");
    assert_eq!(result["body"]["claims"][1]["rule"], "single_solid");
    assert_eq!(result["body"]["claims"][1]["status"], "met");
}

#[tokio::test]
async fn e6_h7_fit_checks_the_compiled_feature_on_the_worker() {
    let p = open("housing").await;
    let id = p.approve(E6).await;
    let result = p.run(&id).await;

    assert_eq!(result["status"], "met", "{result}");
    assert_eq!(result["body"]["claims"][0]["rule"], "fit");
    assert_eq!(result["body"]["primitives"][0]["primitive"], "diameter");
    let worst = &result["body"]["primitives"][0]["worst"];
    approx(worst["value"]["value"].as_f64().unwrap(), 8.007);
    approx(worst["margin"]["value"].as_f64().unwrap(), 0.007);
    assert_eq!(worst["pair"]["a"][0], "housing.py#housing/housing");
}

#[tokio::test]
async fn proxy_joint_measurement_runs_through_approved_simulation_trace() {
    let p = open("proxy").await;
    let parent = p.core.get_project(&p.workspace).await.unwrap();
    let (view, initial_build) = p
        .core
        .open_project_model(
            &p.workspace,
            parent["revision"].as_str().unwrap(),
            "slider.py",
            "slider",
            "user",
        )
        .await
        .unwrap();
    let initial = p.wait_for_job(&view, &initial_build.id).await;
    assert_eq!(
        initial.state,
        JobState::Succeeded,
        "{}",
        initial.diagnostics
    );

    p.install_mechanism(
        &view,
        json!({
            "joints": [{
                "id": "slide",
                "parent": null,
                "child": "slider",
                "kind": {"type": "prismatic", "axis": [1.0, 0.0, 0.0]},
                "initial": 0.01,
                "limits": [0.0, 0.05]
            }],
            "collision": {
                "bodies": {"slider": [{"shape": {"type": "bounds"}}]},
                "clearance_m": 0.0,
                "backoff_m": 0.000001,
                "ignored_pairs": []
            }
        }),
    )
    .await;

    // Requirement actions from a model tab use the model view, which owns the
    // staged mechanism and its pinned build revision.
    let mut p = p;
    p.workspace = view;
    let id = p.approve(PROXY1).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "met", "{result}");
    let primitive = &result["body"]["primitives"][0];
    assert_eq!(primitive["primitive"], "joint");
    assert_eq!(primitive["accuracy"], "proxy", "{primitive}");
    assert_eq!(primitive["worst"]["value"]["unit"], "mm", "{primitive}");
    approx(primitive["worst"]["value"]["value"].as_f64().unwrap(), 10.0);
    approx(
        primitive["worst"]["margin"]["value"].as_f64().unwrap(),
        0.001,
    );

    let repeated = p.run(&id).await;
    assert_eq!(repeated["status"], "met", "{repeated}");
    assert_eq!(
        repeated["body"], result["body"],
        "proxy result must be deterministic"
    );
}

const CONTACT_IMPULSE: &str = r#"{
  "ir_version": 1,
  "id": "drop-impulse",
  "name": "Ball registers a contact impulse on the plate",
  "text": "the ball must hit the plate hard enough to register a contact impulse",
  "claims": [
    {
      "id": "c1",
      "rule": "measure",
      "span": [0, 69],
      "args": {
        "fn": { "text": "contact_impulse" },
        "a": { "part": { "model": { "entry": "drop.py", "builder": "drop" }, "part": { "name": "ball" } } },
        "b": { "part": { "model": { "entry": "drop.py", "builder": "drop" }, "part": { "name": "plate" } } },
        "direction": { "axis": { "vector": [0.0, 0.0, -1.0] } },
        "cmp": {
          "compare": {
            "op": ">=",
            "value": { "value": 0.0001, "unit": "N_s", "source": "inferred", "note": "any real contact" }
          }
        },
        "at": {
          "states": {
            "kind": "simulate",
            "model": { "entry": "drop.py", "builder": "drop" },
            "simulation": {
              "initial_coordinates": {},
              "simulation": {
                "gravity_m_s2": [0.0, 0.0, -9.81],
                "bodies": {
                  "ball": {
                    "type": "dynamic",
                    "mass": { "type": "kilograms", "mass_kg": 0.05 },
                    "friction": 0.0,
                    "restitution": 0.0
                  },
                  "plate": { "type": "fixed", "friction": 0.0, "restitution": 0.0 }
                }
              },
              "actions": [ { "ticks": 120, "motors": [], "loads": {} } ]
            },
            "when": { "kind": "eventually" }
          }
        }
      }
    }
  ]
}"#;

#[tokio::test]
async fn contact_impulse_of_a_falling_body_is_sampled_from_the_proxy_trace() {
    let p = open("proxy").await;
    let parent = p.core.get_project(&p.workspace).await.unwrap();
    let (view, initial_build) = p
        .core
        .open_project_model(
            &p.workspace,
            parent["revision"].as_str().unwrap(),
            "drop.py",
            "drop",
            "user",
        )
        .await
        .unwrap();
    let initial = p.wait_for_job(&view, &initial_build.id).await;
    assert_eq!(
        initial.state,
        JobState::Succeeded,
        "{}",
        initial.diagnostics
    );

    // A free dynamic body: no joints, so gravity alone moves the ball down.
    p.install_mechanism(
        &view,
        json!({
            "joints": [],
            "collision": {
                "bodies": {
                    "ball": [{"shape": {"type": "bounds"}}],
                    "plate": [{"shape": {"type": "bounds"}}]
                },
                "clearance_m": 0.0,
                "backoff_m": 0.000001,
                "ignored_pairs": []
            }
        }),
    )
    .await;

    let mut p = p;
    p.workspace = view;
    let id = p.approve(CONTACT_IMPULSE).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "met", "{result}");
    let primitive = &result["body"]["primitives"][0];
    assert_eq!(primitive["primitive"], "contact_impulse");
    assert_eq!(primitive["accuracy"], "proxy", "{primitive}");
    assert_eq!(primitive["quantifier"], "exists", "{primitive}");
    let witness = &primitive["witness"];
    assert_eq!(witness["value"]["unit"], "N_s", "{primitive}");
    // Peak support impulse while the ball rests on the plate (~m g dt).
    let impulse = witness["value"]["value"].as_f64().unwrap();
    assert!((0.003..0.02).contains(&impulse), "{primitive}");
    // `exists` + eventually reports the impact tick, not the resting state.
    let tick = witness["state"].as_u64().unwrap();
    assert!((3..20).contains(&tick), "{primitive}");
    // `worst` is for `for_all` claims only; a witness replaces it.
    assert!(primitive["worst"].is_null(), "{primitive}");
}

#[tokio::test]
async fn feature_axis_rotation_compiles_and_runs_on_the_worker() {
    let p = open("turntable").await;
    // Publish the rotor's cylindrical face as a named axis.
    p.edit(
        "machine.py",
        "context.show(rotor, name=\"rotor\")",
        "context.show(rotor, name=\"rotor\", features={\"rotor_axis\": rotor.faces(\"%CYLINDER\")})",
    );
    let mut ir: Value = serde_json::from_str(R1).unwrap();
    ir["id"] = json!("r1-axis");
    ir["claims"][0]["span"] = Value::Null;
    ir["claims"][0]["args"]["over"]["states"]["axis"] = json!({
        "feature": {
            "kind": "explicit",
            "part": {
                "model": { "entry": "machine.py", "builder": "machine" },
                "part": { "name": "rotor" }
            },
            "name": "rotor_axis"
        }
    });
    ir["claims"][0]["args"]["over"]["states"]["range"]["step"]["value"] = json!(45.0);
    if let Some(answers) = ir.get_mut("answers") {
        *answers = json!([]);
    }
    let id = p.approve(&ir.to_string()).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "met", "{result}");
    let primitive = &result["body"]["primitives"][0];
    assert_eq!(primitive["states"], 9, "{primitive}");
    // Exact accuracy is omitted from the body (it is the default).
    assert!(primitive.get("accuracy").is_none(), "{primitive}");
    approx(
        primitive["worst"]["margin"]["value"].as_f64().unwrap(),
        0.001,
    );
    assert!(
        primitive["worst"]["state_label"]
            .as_str()
            .is_some_and(|label| label.contains("feature axis")),
        "{primitive}"
    );
}

#[tokio::test]
async fn a_missing_feature_axis_blocks_compilation_instead_of_passing() {
    let p = open("turntable").await;
    let mut ir: Value = serde_json::from_str(R1).unwrap();
    ir["id"] = json!("r1-axis-missing");
    ir["claims"][0]["span"] = Value::Null;
    ir["claims"][0]["args"]["over"]["states"]["axis"] = json!({
        "feature": {
            "kind": "explicit",
            "part": {
                "model": { "entry": "machine.py", "builder": "machine" },
                "part": { "name": "rotor" }
            },
            "name": "no_such_axis"
        }
    });
    ir["claims"][0]["args"]["over"]["states"]["range"]["step"]["value"] = json!(45.0);
    if let Some(answers) = ir.get_mut("answers") {
        *answers = json!([]);
    }
    // The snapshot's published features are the symbol table: a missing axis
    // is a binding error, never a silent pass.
    let compiled = p
        .call(None, json!({"action": "compile", "ir": ir}))
        .await
        .unwrap();
    assert_eq!(compiled["compiled"]["ok"], false, "{compiled}");
    let diagnostics = compiled["compiled"]["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics.iter().any(|d| d["code"] == "E_BIND_FEATURE_MISSING"
            && d["at"] == "claims[0].args.over.axis"),
        "{compiled}"
    );
}

#[tokio::test]
async fn approving_a_new_plan_preserves_the_previous_verdict_until_rechecked() {
    let p = open("bracket").await;
    let id = p.approve(E5).await;
    let first = p.run(&id).await;
    let ir = E5.replace("256", "100");
    assert_eq!(p.approve(&ir).await, id);

    let historical = entry(&p.list().await, &id);
    assert_eq!(historical["status"], "pending");
    assert_eq!(historical["rerun_required"], true);
    assert_eq!(historical["last_result"], first);

    let second = p.run(&id).await;
    assert_eq!(second["status"], "not_met");
    assert_ne!(second["plan_hash"], first["plan_hash"]);
    assert_eq!(entry(&p.list().await, &id)["rerun_required"], false);
}

#[tokio::test]
async fn r4_retractable_arms_stay_clear_of_the_envelope() {
    let p = open("clamp").await;
    let id = p.approve(R4).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "met", "{result}");
    // Extension 5..15 is eleven states, ten of them rebuilds of the subject.
    assert_eq!(result["body"]["primitives"][0]["states"], 11);
    approx(margin(&result, 0), 0.001);
    approx(margin(&result, 1), 0.001);
    assert_eq!(result["body"]["claims"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn r2_comb_tooth_blocks_the_clamps_at_full_travel() {
    let p = open("clamp").await;
    let id = p.approve(R2).await;
    let result = p.run(&id).await;
    assert_eq!(result["status"], "not_met", "{result}");
    // 43 angles x 3 comb travels. Only the 8 mm tooth reaches clamp_3, for
    // angles 72..108; worst at 90 where the clamp sits right above it.
    assert_eq!(result["body"]["primitives"][0]["states"], 129);
    approx(margin(&result, 0), -47.999);
    let worst = &result["body"]["claims"][0]["worst"];
    assert_eq!(
        worst["state_label"],
        "clamp_theta = 90°, comb_travel = 8 mm"
    );
    assert_eq!(worst["pair"]["a"][0], "machine.py#machine/clamp_3");
    assert_eq!(worst["pair"]["b"][0], "machine.py#machine/tower_1");
    let findings = result["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 19);
    assert!(findings.iter().all(|f| f["label"] == "new"));
    assert!(findings.iter().all(|f| {
        f["states"][0]["label"]
            .as_str()
            .unwrap()
            .ends_with("comb_travel = 8 mm")
    }));
    assert_eq!(result["body"]["primitives"][0]["failing_state_count"], 19);

    // Fix the planted fault: the tooth no longer reaches the clamp path. The
    // next run resolves every earlier finding.
    p.edit(
        "machine.py",
        "tooth = cq.Workplane().box(4, comb, 4).translate((TOWER_X, 5 + comb / 2, 50))",
        "tooth = cq.Workplane().box(4, comb / 2, 4).translate((TOWER_X, 5 + comb / 4, 50))",
    );
    let fixed = p.run(&id).await;
    assert_eq!(fixed["status"], "met", "{fixed}");
    let labels = finding_labels(&fixed);
    assert_eq!(labels.len(), 19);
    assert!(labels.iter().all(|l| l == "resolved"), "{labels:?}");
    assert_eq!(fixed["relink"]["decision"], "run");
}

// -------------------------------------------------------------- determinism

#[tokio::test]
async fn results_are_identical_across_pool_sizes_and_caches() {
    for (fixture, ir) in [
        ("turntable", R1),
        ("clamp", R2),
        ("cabinet", E3),
        ("bracket", E5),
        ("clamp", R4),
    ] {
        let p = open(fixture).await;
        let id = p.approve(ir).await;
        let mut expected = None;
        for (workers, use_cache) in [
            (Some(0), false),
            (Some(3), false),
            (None, true),
            (Some(1), true),
        ] {
            let opts = RunOptions {
                workers,
                use_cache,
                store: false,
            };
            let outcome = p
                .core
                .evaluate_requirement("user", &p.workspace, &id, &opts)
                .await
                .unwrap();
            let eval = outcome.eval.expect("evaluated");
            assert_eq!(
                eval.body.hash(),
                outcome.result["result_hash"].as_str().unwrap()
            );
            let bytes = serde_json::to_vec(&eval.body).unwrap();
            if let Some(expected) = &expected {
                assert_eq!(
                    &bytes, expected,
                    "{fixture}: workers={workers:?}, cache={use_cache}"
                );
            } else {
                expected = Some(bytes);
            }
        }
        // Nothing was stored: the run was not asked to store.
        assert!(p.list().await["requirements"][0]["last_result"].is_null());
    }
}

#[tokio::test]
async fn contained_worker_runs_in_process_and_repeats_exactly() {
    // Containment forbids fork: the pool is capped to zero, tasks run serially.
    let p = open_with("turntable", true).await;
    let id = p.approve(R1).await;
    let first = p.run(&id).await;
    assert_eq!(first["status"], "met", "{first}");
    let second = p.run(&id).await;
    assert_eq!(first["result_hash"], second["result_hash"]);
    assert_eq!(first["plan_hash"], second["plan_hash"]);
}

// ------------------------------------------------------------------- relink

#[tokio::test]
async fn relink_geometry_only_change_runs_silently() {
    let p = open("turntable").await;
    let id = p.approve(R1).await;
    let first = p.run(&id).await;
    p.edit(
        "machine.py",
        "circle(50).extrude(6)",
        "circle(52).extrude(6)",
    );
    let second = p.run(&id).await;
    assert_eq!(second["status"], "met", "{second}");
    assert_eq!(second["relink"]["decision"], "run");
    assert_eq!(
        second["relink"]["geometry_changed"],
        json!(["machine.py#machine/rotor"])
    );
    assert!(second["relink"]["changes"].as_array().unwrap().is_empty());
    assert!(
        second["changed_since_approval"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // A new build is a new plan and a new result.
    assert_ne!(first["plan_hash"], second["plan_hash"]);
    assert_eq!(second["ran"], true);
}

#[tokio::test]
async fn relink_refactor_with_unchanged_geometry_relinks_by_hash() {
    let p = open("turntable").await;
    let id = p.approve(R1).await;
    let first = p.run(&id).await;
    p.edit(
        "machine.py",
        "context.show(rotor, name=\"rotor\")",
        "context.show(rotor, name=\"platter\")",
    );
    let second = p.run(&id).await;
    assert_eq!(second["status"], "met", "{second}");
    assert_eq!(second["ran"], true);
    assert_eq!(second["relink"]["decision"], "run");
    let relinks = second["relink"]["relinks"].as_array().unwrap();
    assert_eq!(relinks.len(), 1);
    assert_eq!(relinks[0]["from"], "machine.py#machine/rotor");
    assert_eq!(relinks[0]["to"], "machine.py#machine/platter");
    assert!(second["diff"].as_str().unwrap().contains("platter"));
    // Same geometry, same verdict and margin; the plan names the new part.
    approx(margin(&first, 0), margin(&second, 0));
    assert_eq!(second["status"], first["status"]);
    // The approved requirement itself is untouched.
    let listed = p.list().await;
    assert_eq!(listed["requirements"][0]["approval"], "approved");
}

#[tokio::test]
async fn exact_rename_still_reports_other_parts_added_to_the_check() {
    let p = open("turntable").await;
    let mut ir: Value = serde_json::from_str(&r1_step("others", 45.0)).unwrap();
    ir["claims"][0]["args"]["against"] = json!("others");
    let id = p.approve(&ir.to_string()).await;
    assert_eq!(p.run(&id).await["status"], "met");
    p.edit(
        "machine.py",
        "context.show(rotor, name=\"rotor\")",
        "context.show(rotor, name=\"platter\")\n    context.show(cq.Workplane().box(2, 2, 2).translate((150, 0, 0)), name=\"stop\")",
    );
    let result = p.run(&id).await;
    assert_eq!(result["status"], "met", "{result}");
    assert_eq!(result["relink"]["decision"], "run_changed");
    assert_eq!(
        result["relink"]["relinks"][0]["to"],
        "machine.py#machine/platter"
    );
    let changes = result["changed_since_approval"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{changes:?}");
    assert_eq!(changes[0]["kind"], "others_membership");
    assert!(changes[0]["message"].as_str().unwrap().contains("stop"));
}

#[tokio::test]
async fn relink_removed_part_needs_review_and_keeps_the_previous_result() {
    let p = open("turntable").await;
    let id = p.approve(R1).await;
    let first = p.run(&id).await;
    p.edit(
        "machine.py",
        "    context.show(rotor, name=\"rotor\")\n",
        "",
    );
    let second = p.run(&id).await;
    assert_eq!(second["status"], "needs_review", "{second}");
    assert_eq!(second["ran"], false);
    assert_eq!(second["show_previous"], true);
    assert_eq!(second["relink"]["decision"], "needs_review");
    assert_eq!(second["relink"]["changes"][0]["kind"], "reference_gone");
    assert!(second["diff"].as_str().unwrap().contains("rotor"));
    // The previous verdict is still there, labelled as previous.
    assert_eq!(second["body"], first["body"]);
    assert_eq!(second["result_hash"], first["result_hash"]);
    let listed = p.list().await;
    assert_eq!(listed["requirements"][0]["status"], "needs_review");
}

#[tokio::test]
async fn relink_param_bounds_change_runs_with_a_diff() {
    let p = open("cabinet").await;
    let id = p.approve(E3).await;
    let _ = p.run(&id).await;
    p.edit("cabinet.py", "maximum=130.0", "maximum=120.0");
    let second = p.run(&id).await;
    assert_eq!(second["relink"]["decision"], "run_changed");
    assert_eq!(second["relink"]["changes"][0]["kind"], "param_bounds");
    assert_eq!(
        second["changed_since_approval"].as_array().unwrap().len(),
        1
    );
    assert_eq!(second["ran"], true);
    // Still the same door: the verdict stands, with the difference listed.
    assert_eq!(second["status"], "not_met");
    assert_eq!(finding_labels(&second), ["still_failing"]);
}

#[tokio::test]
async fn failing_build_is_an_error_and_keeps_the_previous_result() {
    let p = open("turntable").await;
    let id = p.approve(R1).await;
    let first = p.run(&id).await;
    p.edit(
        "machine.py",
        "def machine(context):\n",
        "def machine(context):\n    raise RuntimeError(\"boom\")\n",
    );
    let second = p.run(&id).await;
    assert_eq!(second["status"], "error", "{second}");
    assert_eq!(second["ran"], false);
    assert_eq!(second["show_previous"], true);
    assert!(
        second["reasons"].to_string().contains("boom"),
        "{}",
        second["reasons"]
    );
    assert_eq!(second["body"], first["body"]);
    // Fix the source: the next run is a verdict again.
    p.edit("machine.py", "    raise RuntimeError(\"boom\")\n", "");
    let third = p.run(&id).await;
    assert_eq!(third["status"], "met", "{third}");
}

// -------------------------------------------------------------------- queue

async fn wait_for_state(p: &Project, id: &str, wanted: &[&str]) -> Value {
    for _ in 0..600 {
        let run = entry(&p.list().await, id)["run"].clone();
        if run.is_object() && wanted.contains(&run["state"].as_str().unwrap()) {
            return run;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("run never reached {wanted:?}");
}

#[tokio::test]
async fn start_run_reports_state_one_run_per_project_and_cancel_keeps_the_previous_result() {
    let p = open("clamp").await;
    let slow = p.approve(R2).await;
    let quick = p.approve(R4).await;
    let first = p.run(&quick).await;
    assert_eq!(first["status"], "met");

    let mut events = p.core.subscribe_activity();
    let started = p
        .call(None, json!({"action": "start_run", "id": slow}))
        .await
        .unwrap();
    let run = &started["run"]["run"];
    assert_eq!(run["requirement_id"], slow.as_str());
    assert!(
        ["queued", "running"].contains(&run["state"].as_str().unwrap()),
        "{run}"
    );
    let run_id = run["id"].as_str().unwrap().to_owned();
    // `list` reports the active run on its requirement, and in `run`.
    let listed = p.list().await;
    assert!(entry(&listed, &slow)["run"]["state"].is_string());
    assert_eq!(listed["run"]["id"], run_id.as_str());
    // One run per project: a second start is refused, retryably.
    let err = p
        .call(None, json!({"action": "start_run", "id": quick}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "RESOURCE_LIMIT");

    let cancelled = p
        .call(None, json!({"action": "cancel_run", "run_id": run_id}))
        .await
        .unwrap();
    assert!(
        ["cancelling", "cancelled"].contains(&cancelled["run"]["state"].as_str().unwrap_or("none")),
        "{}",
        cancelled["run"]
    );
    let done = wait_for_state(&p, &slow, &["cancelled", "failed", "completed"]).await;
    assert_eq!(done["state"], "cancelled", "{done}");
    // Nothing was stored for the cancelled run; the other result is untouched.
    let listed = p.list().await;
    assert!(entry(&listed, &slow)["last_result"].is_null());
    assert_eq!(
        entry(&listed, &quick)["last_result"]["completed_at_ms"],
        first["completed_at_ms"]
    );
    let mut kinds = Vec::new();
    while let Ok(event) = events.try_recv() {
        kinds.push(event.kind);
    }
    assert!(
        kinds.iter().any(|k| k == "requirement_run_changed"),
        "{kinds:?}"
    );

    // The project is free again, and a stale run id cannot stop a new run.
    let again = p
        .call(None, json!({"action": "run", "id": quick}))
        .await
        .unwrap();
    assert_eq!(again["run"]["result"]["status"], "met");
    let err = p
        .call(None, json!({"action": "cancel_run", "run_id": run_id}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "STALE_ENTITY");
}

#[tokio::test]
async fn start_run_completes_and_stores_the_result() {
    let p = open("bracket").await;
    let id = p.approve(E5).await;
    p.call(None, json!({"action": "start_run", "id": id}))
        .await
        .unwrap();
    let done = wait_for_state(&p, &id, &["completed", "failed", "cancelled"]).await;
    assert_eq!(done["state"], "completed", "{done}");
    let listed = p.list().await;
    assert_eq!(listed["requirements"][0]["status"], "met");
    assert_eq!(listed["requirements"][0]["last_result"]["ran"], true);
}

#[tokio::test]
async fn agents_cannot_run_what_the_user_has_not_approved() {
    let p = open("bracket").await;
    let ir: Value = serde_json::from_str(E5).unwrap();
    let token = p.catalog().await;
    p.call(Some(&token), json!({"action": "save", "ir": ir}))
        .await
        .unwrap();
    let err = p
        .call(None, json!({"action": "run", "id": "e5"}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "REQUIREMENT_NOT_APPROVED");
}

// ----------------------------------------------------------------- auto-run

#[tokio::test]
async fn rebuilt_private_views_are_replaced_without_appearing_in_recents() {
    let p = open("bracket").await;
    let id = p.approve(E5).await;
    let original = p
        .core
        .list_workspaces()
        .await
        .into_iter()
        .find(|workspace| workspace != &p.workspace)
        .expect("private compiler view");
    let raw_workspaces = p.core.list_workspaces().await;
    assert!(raw_workspaces.contains(&original));
    let listing = p
        .core
        .dispatch(&agent(), "get_project", json!({"workspace_id": null}))
        .await
        .unwrap();
    let visible = listing["workspace_ids"].as_array().unwrap();
    assert!(visible.contains(&json!(p.workspace)));
    assert!(!visible.contains(&json!(original)));
    assert_eq!(
        p.core.workspace_summaries().await.as_array().unwrap().len(),
        1
    );

    p.edit("bracket.py", "box(120,", "box(400,");
    assert_eq!(p.run(&id).await["status"], "not_met");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while p.core.has_workspace(&original).await {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("superseded private view was retained");
    assert_eq!(p.core.list_workspaces().await.len(), 2);
    assert_eq!(
        p.core.workspace_summaries().await.as_array().unwrap().len(),
        1
    );
}

#[tokio::test]
async fn successful_model_build_checks_the_new_revision_before_save() {
    let p = open("bracket").await;
    let id = p.approve(E5).await;
    let parent = p.core.get_project(&p.workspace).await.unwrap();
    let (view, _) = p
        .core
        .open_project_model(
            &p.workspace,
            parent["revision"].as_str().unwrap(),
            "bracket.py",
            "bracket",
            "user",
        )
        .await
        .unwrap();
    let first = wait_for_state(&p, &id, &["completed", "failed"]).await;
    assert_eq!(first["state"], "completed", "{first}");
    assert_eq!(entry(&p.list().await, &id)["status"], "met");

    let model = p.core.get_project(&view).await.unwrap();
    let revision = model["revision"].as_str().unwrap();
    let source = std::fs::read_to_string(p.root.join("bracket.py")).unwrap();
    let draft = p
        .core
        .update_source(
            &view,
            revision,
            None,
            [("bracket.py".into(), source.replace("box(120,", "box(400,"))].into(),
            "user",
        )
        .await
        .unwrap();
    let job = p
        .core
        .build(&view, revision, &draft.id, "user")
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let job = p.core.get_job(&view, &job.id).await.unwrap();
            assert!(
                !matches!(
                    job.state,
                    cadpad_core::JobState::Cancelled | cadpad_core::JobState::Failed
                ),
                "{:?}",
                job.diagnostics
            );
            let requirement = entry(&p.list().await, &id);
            if requirement["status"] == "not_met" {
                assert_eq!(job.state, cadpad_core::JobState::Succeeded);
                assert_eq!(requirement["last_result"]["ran"], true);
                assert_ne!(requirement["run"]["id"], first["id"]);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("auto-run did not report the widened bracket");
    assert_eq!(
        std::fs::read_to_string(p.root.join("bracket.py")).unwrap(),
        source
    );
}

#[tokio::test]
async fn model_builds_record_whether_an_edit_was_naming_or_metadata_only() {
    let p = open("turntable").await;
    let parent = p.core.get_project(&p.workspace).await.unwrap();
    let (view, first) = p
        .core
        .open_project_model(
            &p.workspace,
            parent["revision"].as_str().unwrap(),
            "machine.py",
            "machine",
            "user",
        )
        .await
        .unwrap();
    let job = p.wait_for_job(&view, &first.id).await;
    assert_eq!(job.state, JobState::Succeeded, "{}", job.diagnostics);
    // The first build of a view has no earlier geometry to compare against.
    assert_eq!(
        job.diagnostics["auto_edit"]["decision"], "needs_review",
        "{}",
        job.diagnostics
    );

    let source = std::fs::read_to_string(p.root.join("machine.py")).unwrap();
    // Naming/metadata only: publish a face name. Every part's geometry hash
    // is unchanged, so the edit may be applied without review.
    let named = source.replace(
        "context.show(base, name=\"base\")",
        "context.show(base, name=\"base\", features={\"base_top\": base.faces(\">Z\")})",
    );
    let guard = rebuild_source(&p, &view, "machine.py", &named).await;
    assert_eq!(guard["decision"], "apply", "{guard}");

    // A geometry change is a normal code change, with the part named.
    let moved = source.replace("circle(50).extrude(6)", "circle(52).extrude(6)");
    let guard = rebuild_source(&p, &view, "machine.py", &moved).await;
    assert_eq!(guard["decision"], "needs_review", "{guard}");
    assert!(
        guard["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("rotor")),
        "{guard}"
    );
}

#[tokio::test]
async fn auto_run_reruns_cheap_requirements_and_leaves_dear_ones_pending() {
    let p = open("turntable").await;
    let cheap = p.approve(&r1_step("cheap", 45.0)).await;
    let dear = p.approve(&r1_step("dear", 1.0)).await;
    let mut events = p.core.subscribe_activity();
    let report = p
        .core
        .auto_run_requirements("user", &p.workspace)
        .await
        .unwrap();
    assert_eq!(report.budget_s, 10);
    assert_eq!(report.ran, std::slice::from_ref(&cheap));
    assert_eq!(report.stale.len(), 1);
    assert_eq!(report.stale[0].id, dear);
    assert!(report.stale[0].estimate_s > 10.0);
    assert!(
        report.failed.is_empty() && report.blocked.is_empty(),
        "{report:?}"
    );

    let listed = p.list().await;
    let cheap_result = &entry(&listed, &cheap)["last_result"];
    assert_eq!(cheap_result["status"], "met");
    assert_eq!(cheap_result["ran"], true);
    assert_eq!(cheap_result["queued"], false);
    assert!(cheap_result["estimate_s"].as_f64().unwrap() <= 10.0);
    let dear_entry = entry(&listed, &dear);
    assert_eq!(dear_entry["status"], "pending");
    let dear_result = &dear_entry["last_result"];
    assert_eq!(dear_result["stale"], true);
    assert_eq!(dear_result["ran"], false);
    assert!(dear_result["body"].is_null());
    assert!(
        dear_result["reasons"][0]
            .as_str()
            .unwrap()
            .contains("auto-run budget")
    );
    let mut kinds = Vec::new();
    while let Ok(event) = events.try_recv() {
        kinds.push(event.kind);
    }
    assert!(
        kinds.iter().any(|k| k == "requirements_changed"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|k| k == "requirement_run_changed"),
        "{kinds:?}"
    );

    // On demand: the dear one runs, and the verdict replaces "pending".
    let result = p.run(&dear).await;
    assert_eq!(result["status"], "met");
    assert_eq!(result["body"]["primitives"][0]["states"], 361);

    // A geometry change makes the dear one pending again, showing the
    // previous verdict; the cheap one re-runs. Nothing is lost.
    p.edit(
        "machine.py",
        "circle(50).extrude(6)",
        "circle(52).extrude(6)",
    );
    let report = p
        .core
        .auto_run_requirements("user", &p.workspace)
        .await
        .unwrap();
    assert_eq!(report.ran, std::slice::from_ref(&cheap));
    assert_eq!(report.stale[0].id, dear);
    let listed = p.list().await;
    let dear_entry = entry(&listed, &dear);
    assert_eq!(dear_entry["status"], "pending");
    assert_eq!(dear_entry["last_result"]["show_previous"], true);
    assert_eq!(dear_entry["last_result"]["body"]["status"], "met");

    // Nothing changed since: a repeat of the cheap one is fully cached.
    let report = p
        .core
        .auto_run_requirements("user", &p.workspace)
        .await
        .unwrap();
    assert!(report.ran.contains(&cheap));
    let listed = p.list().await;
    assert_eq!(entry(&listed, &cheap)["last_result"]["estimate_s"], 1.0);
}

#[tokio::test]
async fn auto_run_stores_needs_review_without_running() {
    let p = open("turntable").await;
    let id = p.approve(&r1_step("cheap", 45.0)).await;
    p.edit(
        "machine.py",
        "    context.show(rotor, name=\"rotor\")\n",
        "",
    );
    let report = p
        .core
        .auto_run_requirements("user", &p.workspace)
        .await
        .unwrap();
    assert_eq!(report.blocked, std::slice::from_ref(&id));
    assert!(report.ran.is_empty());
    let listed = p.list().await;
    assert_eq!(listed["requirements"][0]["status"], "needs_review");
}

//! Migration of saved motion tests into compiled requirements (P4.4–P4.5).
//!
//! [`migrate`] runs inside [`Core::requirement_catalog`], the single funnel
//! every requirement operation reads through. Saved tests that name a project
//! model (`definition.model`) are converted to requirement drafts on read and
//! the converted catalog is persisted through the usual compare-and-swap, so
//! the migration happens once. `control_motion_tests` is retired: tests
//! without a model (the addressed design itself, or standalone workspaces,
//! which have no project model catalog) remain as legacy entries. They cannot
//! be compiled as requirements, but retaining them keeps portable project
//! catalogs lossless for older clients and future migration support.
//!
//! Check mapping: `Displacement` → `measure displacement >=`, `Coordinate` →
//! `measure joint approx`, `Push` → displacement, additional push, contact, and
//! sampled contact impulse claims over the trace.
//! Every migrated literal is `inferred`: the values came from the saved test,
//! not from the requirement prose, and approving the printed draft is what
//! confirms them.
//!
//! `last_run` carry-over (P4.5): the outcome is kept as `last_result` only
//! when the run's engine, definition fingerprint and the model view's input
//! fingerprint all still match, so a stale pass is never shown as current.
//! The stored value is history, not a requirements-run-v1 body: it carries no
//! `body`, so result readers ignore it, but its `status` lets
//! [`RequirementEntry::status`] report the old verdict until the approved
//! plan runs (approval marks it `rerun_required` regardless).
use crate::motion_tests::{self, Catalog, Check, Saved, SavedTest, View, engine, fingerprint};
use crate::requirements::ir::{
    Arg, AxisRef, Claim, Comparator, JointRef, Literal, ModelRef, PartName, PartRef, Quantifier,
    Requirement, Source, StateSpace, When,
};
use crate::requirements::quantity::Unit;
use crate::requirements::storage::{EntryKind, RequirementEntry, settings_in};
use crate::requirements::{IR_VERSION, compile, tool::snapshot_for};
use crate::{Core, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn test_model(model: &motion_tests::ModelRef) -> ModelRef {
    ModelRef {
        entry: model.entry.clone(),
        builder: model.builder.clone(),
    }
}

/// A migrated number: no prose span can vouch for it, so it is `inferred`
/// and the note records where it came from. Approving the print confirms it.
fn literal(value: f64, unit: Unit) -> Literal {
    Literal {
        value,
        unit,
        source: Source::Inferred,
        note: Some("from the saved motion test".into()),
    }
}

fn part(model: &ModelRef, name: &str) -> Arg {
    Arg::Part(PartRef {
        model: model.clone(),
        part: PartName::Name(name.to_owned()),
    })
}

fn joint(model: &ModelRef, name: &str) -> Arg {
    Arg::Joint(JointRef {
        model: model.clone(),
        name: name.to_owned(),
    })
}

fn states(space: StateSpace) -> Arg {
    Arg::States(Box::new(space))
}

/// The `simulate` state space shared by every claim of the requirement,
/// rebuilt per `when` selection.
fn simulate(model: &ModelRef, definition: &motion_tests::Definition, when: When) -> StateSpace {
    StateSpace::Simulate {
        model: model.clone(),
        simulation: motion_tests::SimulationState {
            initial_coordinates: definition.initial_coordinates.clone(),
            simulation: definition.simulation.clone(),
            actions: definition.actions.clone(),
        },
        when,
    }
}

fn measure_claim(id: String, note: String, args: Vec<(&str, Arg)>) -> Claim {
    Claim {
        id,
        rule: "measure".into(),
        args: args.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        quantifier: Quantifier::ForAll,
        span: None,
        source: Source::Inferred,
        note: Some(note),
    }
}

fn migrated_note(check: &Check) -> String {
    format!(
        "Migrated from saved motion test check: {}",
        motion_tests::check_statement(check)
    )
}

/// `Displacement` → final projected displacement at least `minimum_mm`.
fn displacement_claim(
    id: String,
    body: &str,
    direction: [f64; 3],
    minimum_mm: f64,
    model: &ModelRef,
    definition: &motion_tests::Definition,
    note: String,
) -> Claim {
    measure_claim(
        id,
        note,
        vec![
            ("fn", Arg::Text("displacement".into())),
            ("body", part(model, body)),
            ("direction", Arg::Axis(AxisRef::Vector(direction))),
            ("at", states(simulate(model, definition, When::End {}))),
            (
                "cmp",
                Arg::Compare(Comparator::Ge {
                    value: literal(minimum_mm, Unit::Mm),
                }),
            ),
        ],
    )
}

/// Build the requirement IR for one saved test. `None` when the test has no
/// project model: there is nothing to bind the simulation to.
pub(crate) fn test_to_requirement(id: &str, test: &SavedTest) -> Option<Requirement> {
    let definition = &test.definition;
    let model = test_model(definition.model.as_ref()?);
    let mut claims = Vec::new();
    for check in definition.checks.iter() {
        let note = migrated_note(check);
        match check {
            Check::Displacement {
                body,
                direction,
                minimum_mm,
            } => claims.push(displacement_claim(
                format!("c{}", claims.len()),
                body,
                *direction,
                *minimum_mm,
                &model,
                definition,
                note,
            )),
            Check::Coordinate {
                joint_id,
                target,
                unit,
                tolerance,
            } => {
                // `joint` reports metres as millimetres and radians as
                // radians; the old check's `unit` selects the same output.
                let (target, tolerance, unit) = match unit.as_str() {
                    "m" => (target * 1000.0, tolerance * 1000.0, Unit::Mm),
                    _ => (*target, *tolerance, Unit::Rad),
                };
                claims.push(measure_claim(
                    format!("c{}", claims.len()),
                    note,
                    vec![
                        ("fn", Arg::Text("joint".into())),
                        ("j", joint(&model, joint_id)),
                        ("at", states(simulate(&model, definition, When::End {}))),
                        (
                            "cmp",
                            Arg::Compare(Comparator::Approx {
                                target: literal(target, unit),
                                tol: literal(tolerance, unit),
                            }),
                        ),
                    ],
                ));
            }
            Check::Push {
                pusher,
                receiver,
                driver_joint,
                direction,
                minimum_mm,
                minimum_additional_mm,
                minimum_sampled_impulse_ns,
            } => {
                let base = claims.len();
                claims.push(displacement_claim(
                    format!("c{base}"),
                    receiver,
                    *direction,
                    *minimum_mm,
                    &model,
                    definition,
                    format!("{note} (receiver displacement)"),
                ));
                claims.push(measure_claim(
                    format!("c{}", base + 1),
                    format!("{note} (additional displacement with {driver_joint} enabled)"),
                    vec![
                        ("fn", Arg::Text("push".into())),
                        ("pusher", part(&model, pusher)),
                        ("receiver", part(&model, receiver)),
                        ("driver_joint", joint(&model, driver_joint)),
                        ("direction", Arg::Axis(AxisRef::Vector(*direction))),
                        ("at", states(simulate(&model, definition, When::End {}))),
                        (
                            "cmp",
                            Arg::Compare(Comparator::Ge {
                                value: literal(*minimum_additional_mm, Unit::Mm),
                            }),
                        ),
                    ],
                ));
                claims.push(measure_claim(
                    format!("c{}", base + 2),
                    format!("{note} (contact presence)"),
                    vec![
                        ("fn", Arg::Text("contact".into())),
                        ("a", part(&model, pusher)),
                        ("b", part(&model, receiver)),
                        (
                            "at",
                            states(simulate(&model, definition, When::Eventually {})),
                        ),
                        ("cmp", Arg::Compare(Comparator::IsTrue {})),
                    ],
                ));
                let mut impulse = measure_claim(
                    format!("c{}", base + 3),
                    format!("{note} (peak sampled contact impulse)"),
                    vec![
                        ("fn", Arg::Text("contact_impulse".into())),
                        ("a", part(&model, pusher)),
                        ("b", part(&model, receiver)),
                        ("direction", Arg::Axis(AxisRef::Vector(*direction))),
                        (
                            "at",
                            states(simulate(&model, definition, When::Eventually {})),
                        ),
                        (
                            "cmp",
                            Arg::Compare(Comparator::Ge {
                                value: literal(*minimum_sampled_impulse_ns, Unit::Ns),
                            }),
                        ),
                    ],
                );
                impulse.quantifier = Quantifier::Exists;
                claims.push(impulse);
            }
        }
    }
    Some(Requirement {
        ir_version: IR_VERSION,
        id: id.to_owned(),
        name: definition.name.clone(),
        text: if definition.intent.trim().is_empty() {
            definition.name.clone()
        } else {
            definition.intent.clone()
        },
        sets: BTreeMap::new(),
        covers: vec![],
        claims,
        allow: vec![],
        unsupported: vec![],
        answers: vec![],
    })
}

/// The carried-over result, only when the run still describes the current
/// design and engine (P4.5). `views` are the addressed workspace plus every
/// open model view of the project; the test's own model must be among them.
fn carried_result(test: &SavedTest, views: &[View]) -> Option<Value> {
    let run = test.last_run.as_ref()?;
    let model = test.definition.model.as_ref()?;
    if run.engine != engine() || run.definition_fingerprint != fingerprint(&test.definition).ok()? {
        return None;
    }
    let view = views
        .iter()
        .find(|view| view.model.as_ref() == Some(model))?;
    if view.inputs != run.input_fingerprint {
        return None;
    }
    Some(json!({
        "format": "motion-test-run-v1",
        "status": if run.outcome.passed { "met" } else { "not_met" },
        "engine": run.engine,
        "input_fingerprint": run.input_fingerprint,
        "definition_fingerprint": run.definition_fingerprint,
        "revision": run.revision,
        "outcome": run.outcome,
    }))
}

fn empty_entry(requirement: Requirement) -> RequirementEntry {
    RequirementEntry {
        kind: EntryKind::Requirement,
        draft: requirement,
        draft_diagnostics: vec![],
        draft_print: None,
        draft_hashes: None,
        approved: None,
        last_result: None,
        rerun_required: false,
    }
}

fn remove_migrated_tests(catalog: &mut Catalog, converted: &[&(String, SavedTest)]) {
    for (id, _) in converted {
        catalog.remove(id);
    }
}

/// Convert saved motion tests into requirement drafts (model-bound), drop
/// model-less tests as orphans, compile drafts against the current build, carry
/// over fresh `last_run` results, and persist the converted catalog.
/// Returns the catalog readers should see: the migrated one after a
/// successful commit, the raw one when migration cannot be persisted (it is
/// retried on the next read).
pub(crate) async fn migrate(
    core: &Core,
    actor: &str,
    addressed: &View,
    catalog: Catalog,
) -> Result<Catalog> {
    let tests: Vec<(String, SavedTest)> = catalog
        .iter()
        .filter_map(|(id, saved)| match saved {
            Saved::Test(test) => Some((id.clone(), test.clone())),
            _ => None,
        })
        .collect();
    if tests.is_empty() {
        return Ok(catalog);
    }
    // Model-less tests have no home in the requirement catalog: they are
    // dropped as orphans on every migration, even when they are the only
    // legacy entries (spec Appendix D).
    let orphans: Vec<String> = tests
        .iter()
        .filter(|(_, test)| test.definition.model.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    let convertible: Vec<&(String, SavedTest)> = tests
        .iter()
        .filter(|(_, test)| test.definition.model.is_some())
        .collect();
    let settings = settings_in(&catalog);
    // One snapshot covers every model of the project; it is built once for all
    // pending tests. Without it the drafts are still stored, uncompiled, so a
    // later agent save can fill the print and hashes.
    let snapshot = match convertible.first() {
        Some((id, test)) => match test_to_requirement(id, test) {
            Some(ir) => snapshot_for(core, &addressed.id, actor, &ir).await.ok(),
            None => None,
        },
        None => None,
    };
    // Views that can vouch for a last_run's input fingerprint.
    let mut views = vec![addressed.clone()];
    for id in core.list_workspaces().await {
        if id == addressed.id || views.iter().any(|view| view.id == id) {
            continue;
        }
        if core.model_parent(&id).await.as_deref() == Some(addressed.owner.as_str())
            && let Ok(view) = core.requirement_view(&id).await
        {
            views.push(view);
        }
    }
    let mut next = catalog.clone();
    for id in &orphans {
        next.remove(id);
    }
    // Only tests that can be faithfully bound to a project model become
    // requirement drafts; the rest were just dropped as orphans.
    remove_migrated_tests(&mut next, &convertible);
    for (id, test) in &convertible {
        let Some(requirement) = test_to_requirement(id, test) else {
            continue;
        };
        let mut entry = match &snapshot {
            Some(snapshot) => {
                let compiled = compile(&requirement, snapshot, &settings);
                RequirementEntry::new_draft(&compiled)
            }
            None => empty_entry(requirement),
        };
        entry.last_result = carried_result(test, &views);
        next.insert(id.clone(), Saved::Requirement(Box::new(entry)));
    }
    // Over capacity or over the entry limit: keep the legacy catalog rather
    // than silently dropping tests; the next read retries.
    if motion_tests::encode(&next).is_err() {
        return Ok(catalog);
    }
    let expected = fingerprint(&catalog)?;
    match core
        .commit_requirement_catalog(
            actor,
            &addressed.owner,
            &addressed.id,
            &expected,
            &next,
            None,
        )
        .await
    {
        Ok(()) => Ok(next),
        // A concurrent catalog write won the CAS; serve what is stored. The
        // migration is retried on the next requirement read.
        Err(_) => Ok(catalog),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition() -> motion_tests::Definition {
        serde_json::from_value(json!({
            "name":"Lever pushes shoe",
            "intent":"Lever must reach and push the shoe by at least 5 mm",
            "model":{"entry":"lever.py","builder":"lever"},
            "initial_coordinates":{"lever_hinge":0,"shoe_slide":0},
            "simulation":{"gravity_m_s2":[0,0,0],"bodies":{
                "lever":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.2},"friction":0,"restitution":0},
                "shoe":{"type":"dynamic","mass":{"type":"kilograms","mass_kg":0.1},"friction":0,"restitution":0}
            }},
            "actions":[{"ticks":120,"motors":[{"joint_id":"lever_hinge","target":0.3,"unit":"rad","stiffness":0.5,"damping":0.03,"max_force":0.2}],"loads":{"shoe":{"force_n":[0,-0.5,0],"torque_nm":[0,0,0]}}}],
            "checks":[
                {"type":"push","pusher":"lever","receiver":"shoe","driver_joint":"lever_hinge","direction":[0,1,0],"minimum_mm":5,"minimum_additional_mm":5,"minimum_sampled_impulse_ns":0.000001},
                {"type":"coordinate","joint_id":"shoe_slide","target":0.01,"unit":"m","tolerance":0.0005},
                {"type":"displacement","body":"shoe","direction":[0,-1,0],"minimum_mm":2}
            ]
        }))
        .unwrap()
    }

    fn saved() -> SavedTest {
        SavedTest {
            definition: definition(),
            last_run: None,
        }
    }

    #[test]
    fn displacement_maps_to_measure_ge() {
        let mut test = saved();
        test.definition.checks = vec![Check::Displacement {
            body: "shoe".into(),
            direction: [0.0, -1.0, 0.0],
            minimum_mm: 2.0,
        }];
        let ir = test_to_requirement("t", &test).unwrap();
        assert_eq!(ir.id, "t");
        assert_eq!(ir.claims.len(), 1);
        let claim = &ir.claims[0];
        assert_eq!(claim.rule, "measure");
        assert_eq!(claim.args["fn"], Arg::Text("displacement".into()));
        assert_eq!(
            claim.args["cmp"],
            Arg::Compare(Comparator::Ge {
                value: literal(2.0, Unit::Mm)
            })
        );
        let at = match &claim.args["at"] {
            Arg::States(space) => (**space).clone(),
            other => panic!("at must be states, got {other:?}"),
        };
        assert!(matches!(
            at,
            StateSpace::Simulate {
                when: When::End {},
                ..
            }
        ));
        // Migrated literals are inferred until the user approves the print.
        assert_eq!(claim.source, Source::Inferred);
        assert!(ir.validate().is_empty(), "{:?}", ir.validate());
    }

    #[test]
    fn coordinate_maps_to_joint_approx_in_output_units() {
        let mut test = saved();
        test.definition.checks = vec![
            Check::Coordinate {
                joint_id: "shoe_slide".into(),
                target: 0.01,
                unit: "m".into(),
                tolerance: 0.0005,
            },
            Check::Coordinate {
                joint_id: "lever_hinge".into(),
                target: 0.3,
                unit: "rad".into(),
                tolerance: 0.017,
            },
        ];
        let ir = test_to_requirement("t", &test).unwrap();
        assert_eq!(ir.claims.len(), 2);
        let metres = &ir.claims[0];
        assert_eq!(metres.args["fn"], Arg::Text("joint".into()));
        assert_eq!(
            metres.args["cmp"],
            Arg::Compare(Comparator::Approx {
                target: literal(10.0, Unit::Mm),
                tol: literal(0.5, Unit::Mm),
            })
        );
        let radians = &ir.claims[1];
        assert_eq!(
            radians.args["cmp"],
            Arg::Compare(Comparator::Approx {
                target: literal(0.3, Unit::Rad),
                tol: literal(0.017, Unit::Rad),
            })
        );
        assert!(ir.validate().is_empty(), "{:?}", ir.validate());
    }

    #[test]
    fn push_maps_to_displacement_push_and_eventual_contact() {
        let mut test = saved();
        test.definition.checks = vec![Check::Push {
            pusher: "lever".into(),
            receiver: "shoe".into(),
            driver_joint: "lever_hinge".into(),
            direction: [0.0, 1.0, 0.0],
            minimum_mm: 5.0,
            minimum_additional_mm: 5.0,
            minimum_sampled_impulse_ns: 0.000001,
        }];
        let ir = test_to_requirement("t", &test).unwrap();
        assert_eq!(ir.claims.len(), 4);
        let fns: Vec<&str> = ir
            .claims
            .iter()
            .map(|c| match &c.args["fn"] {
                Arg::Text(name) => name.as_str(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(fns, ["displacement", "push", "contact", "contact_impulse"]);
        let push = &ir.claims[1];
        assert_eq!(
            push.args["driver_joint"],
            joint(
                &ModelRef {
                    entry: "lever.py".into(),
                    builder: "lever".into()
                },
                "lever_hinge"
            )
        );
        assert_eq!(
            push.args["cmp"],
            Arg::Compare(Comparator::Ge {
                value: literal(5.0, Unit::Mm)
            })
        );
        let contact = &ir.claims[2];
        assert_eq!(contact.args["cmp"], Arg::Compare(Comparator::IsTrue {}));
        let at = match &contact.args["at"] {
            Arg::States(space) => (**space).clone(),
            other => panic!("at must be states, got {other:?}"),
        };
        assert!(matches!(
            at,
            StateSpace::Simulate {
                when: When::Eventually {},
                ..
            }
        ));
        let impulse = &ir.claims[3];
        assert_eq!(impulse.quantifier, Quantifier::Exists);
        assert_eq!(impulse.args["fn"], Arg::Text("contact_impulse".into()));
        assert_eq!(
            impulse.args["direction"],
            Arg::Axis(AxisRef::Vector([0.0, 1.0, 0.0]))
        );
        assert_eq!(
            impulse.args["cmp"],
            Arg::Compare(Comparator::Ge {
                value: literal(0.000001, Unit::Ns)
            })
        );
        assert!(impulse.note.as_deref().unwrap().contains("peak sampled"));
        assert!(ir.validate().is_empty(), "{:?}", ir.validate());
    }

    #[test]
    fn model_less_tests_do_not_migrate() {
        let mut test = saved();
        test.definition.model = None;
        assert!(test_to_requirement("t", &test).is_none());

        // Portable catalogs can contain addressed-design/standalone motion
        // tests. Keep the legacy entry byte-compatible when other tests are
        // migrated, instead of silently dropping its saved definition/run.
        let mut bound = saved();
        bound.definition.name = "Bound".into();
        let mut catalog = Catalog::new();
        catalog.insert("portable".into(), Saved::Test(test.clone()));
        catalog.insert("bound".into(), Saved::Test(bound));
        let tests: Vec<(String, SavedTest)> = catalog
            .iter()
            .filter_map(|(id, saved)| match saved {
                Saved::Test(test) => Some((id.clone(), test.clone())),
                _ => None,
            })
            .collect();
        let convertible: Vec<_> = tests
            .iter()
            .filter(|(_, test)| test.definition.model.is_some())
            .collect();
        let mut next = catalog.clone();
        remove_migrated_tests(&mut next, &convertible);
        assert!(matches!(next.get("portable"), Some(Saved::Test(_))));
        assert!(!next.contains_key("bound"));
        // The retained legacy entry survives the normal portable encoding.
        let bytes = motion_tests::encode(&next).unwrap();
        let decoded: Catalog = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(decoded.get("portable"), Some(Saved::Test(_))));
    }

    /// A built snapshot of the fixture model: published parts `lever`/`shoe`
    /// and the two mechanism joints the test drives.
    fn snapshot() -> crate::requirements::WorkspaceSnapshot {
        use crate::requirements::snapshot::{
            BuiltModel, JointInfo, ModelSnapshot, PartInfo, WorkspaceSnapshot,
        };
        let part = |name: &str| PartInfo {
            name: name.into(),
            geometry_hash: "h".into(),
            source: None,
            fastener: None,
            source_assets: vec![],
        };
        WorkspaceSnapshot {
            models: vec![ModelSnapshot {
                model: ModelRef {
                    entry: "lever.py".into(),
                    builder: "lever".into(),
                },
                label: None,
                build_error: None,
                built: Some(BuiltModel {
                    fingerprint: "f".into(),
                    parts: vec![part("lever"), part("shoe")],
                    params: vec![],
                    joints: vec![
                        JointInfo {
                            name: "lever_hinge".into(),
                            unit: Some("rad".into()),
                            limits: Some([0.0, 1.0]),
                        },
                        JointInfo {
                            name: "shoe_slide".into(),
                            unit: Some("m".into()),
                            limits: Some([0.0, 0.04]),
                        },
                    ],
                    ..Default::default()
                }),
            }],
        }
    }

    #[test]
    fn migrated_draft_compiles_against_the_model_build() {
        let test = saved();
        let ir = test_to_requirement("t", &test).unwrap();
        let compiled = compile(
            &ir,
            &snapshot(),
            &crate::requirements::ProjectSettings::default(),
        );
        assert!(
            !compiled.has_errors(),
            "{:?}",
            compiled
                .diagnostics
                .iter()
                .filter(|d| d.is_error())
                .collect::<Vec<_>>()
        );
        // The print and hashes a review would approve exist.
        assert!(compiled.print.is_some() && compiled.hashes.is_some());
        let primitives: Vec<&str> = compiled
            .primitives
            .iter()
            .map(|p| p.primitive.as_str())
            .collect();
        assert_eq!(
            primitives,
            [
                "displacement",
                "push",
                "contact",
                "contact_impulse",
                "joint",
                "displacement"
            ]
        );
    }

    #[test]
    fn migrated_ir_serializes_and_loads_as_a_requirement_entry() {
        let test = saved();
        let ir = test_to_requirement("t", &test).unwrap();
        let entry = RequirementEntry {
            kind: EntryKind::Requirement,
            draft: ir,
            draft_diagnostics: vec![],
            draft_print: None,
            draft_hashes: None,
            approved: None,
            last_result: None,
            rerun_required: false,
        };
        let mut catalog = Catalog::new();
        catalog.insert("t".to_owned(), Saved::Requirement(Box::new(entry)));
        let bytes = motion_tests::encode(&catalog).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["t"]["kind"], "requirement");
        assert!(value["t"]["draft"]["claims"].as_array().unwrap().len() >= 5);
    }
}

//! Query math tests use synthetic coverage metadata. Real BRep coverage is tested
//! separately through the supervised worker in collision_integration.rs.
use cadpad_core::{
    collision::World,
    kinematics::{self, Mechanism},
};
use serde_json::{Value, json};
fn box_piece(half: [f64; 3]) -> Value {
    json!({"shape":{"type":"box","half_extents_m":half}})
}
fn fixture(kind: Value, axis: [f64; 3], wall: [f64; 3], bodies: Value) -> (Mechanism, World) {
    let model = json!({"parts":[{"instance_id":"mover","position":[0,0,0]},{"instance_id":"wall","position":wall}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["mover","wall"]}});
    let mut kind = kind;
    kind["axis"] = json!(axis);
    let definition=kinematics::decode(&json!({"joints":[{"id":"drive","parent":null,"child":"mover","kind":kind,"initial":0,"limits":[-60,60]}],"collision":{"bodies":bodies,"clearance_m":0,"backoff_m":0.000001}})).unwrap();
    let mechanism = Mechanism::new(definition.clone(), &model["parts"]).unwrap();
    let world = World::new(definition.collision.unwrap(), &model).unwrap();
    (mechanism, world)
}
#[test]
fn thin_obstacle_blocks_a_slider_even_when_the_requested_endpoint_is_clear() {
    let bodies = json!({"mover":[box_piece([0.005;3])],"wall":[box_piece([0.0001,0.02,0.02])]});
    let (mechanism, world) = fixture(
        json!({"type":"prismatic"}),
        [1., 0., 0.],
        [50., 0., 0.],
        bodies,
    );
    let hit = world.sweep(&mechanism, "drive", 0.1).unwrap();
    assert!(hit.blocked);
    assert!((hit.fraction - 0.44899).abs() < 1e-6, "{hit:?}");
    let mut endpoint = mechanism.clone();
    endpoint.set("drive", 0.1, "m").unwrap();
    assert!(
        world
            .report(&endpoint.evaluate().unwrap())
            .unwrap()
            .contacts
            .is_empty()
    );
    assert_eq!(mechanism.evaluate().unwrap().joints[0].coordinate, 0.);
}
#[test]
fn a_full_hinge_turn_checks_its_arc_instead_of_matching_identical_endpoints() {
    let bodies = json!({"mover":[{"frame":{"position_m":[0.05,0,0]},"shape":{"type":"box","half_extents_m":[0.004,0.002,0.002]}}],"wall":[{"shape":{"type":"ball","radius_m":0.003}}]});
    let (mechanism, world) = fixture(
        json!({"type":"revolute"}),
        [0., 0., 1.],
        [0., 50., 0.],
        bodies,
    );
    let hit = world
        .sweep(&mechanism, "drive", std::f64::consts::TAU)
        .unwrap();
    assert!(hit.blocked, "{hit:?}");
    let angle = hit.fraction * std::f64::consts::TAU;
    assert!(angle > 1.3 && angle < 1.6, "{hit:?}");
    let reverse = world
        .sweep(&mechanism, "drive", -std::f64::consts::TAU)
        .unwrap();
    assert!(reverse.blocked);
    assert!(reverse.fraction > 0.7 && reverse.fraction < 0.8);
    assert_eq!(
        world.sweep(&mechanism, "drive", 60.).unwrap_err().code(),
        "COLLISION_SWEEP_LIMIT"
    );
}
#[test]
fn initial_touch_allows_tangential_and_separating_translation_but_not_penetration() {
    let bodies = json!({"mover":[box_piece([0.005;3])],"wall":[box_piece([0.005;3])]});
    let (tangent, world) = fixture(
        json!({"type":"prismatic"}),
        [1., 0., 0.],
        [0., 10., 0.],
        bodies.clone(),
    );
    assert!(!world.sweep(&tangent, "drive", 0.03).unwrap().blocked);
    let (normal, world) = fixture(
        json!({"type":"prismatic"}),
        [0., 1., 0.],
        [0., 10., 0.],
        bodies,
    );
    assert!(!world.sweep(&normal, "drive", -0.03).unwrap().blocked);
    let hit = world.sweep(&normal, "drive", 0.03).unwrap();
    assert!(hit.blocked);
    assert_eq!(hit.fraction, 0.);
}
#[test]
fn compound_proxies_preserve_a_channel_that_a_single_box_would_fill() {
    let rail = json!([{"frame":{"position_m":[0,0,-0.005]},"shape":{"type":"box","half_extents_m":[0.1,0.01,0.002]}},{"frame":{"position_m":[0,0.008,0.003]},"shape":{"type":"box","half_extents_m":[0.1,0.002,0.006]}},{"frame":{"position_m":[0,-0.008,0.003]},"shape":{"type":"box","half_extents_m":[0.1,0.002,0.006]}}]);
    let bodies = json!({"mover":[box_piece([0.01,0.004,0.002])],"wall":rail});
    let (mechanism, world) = fixture(
        json!({"type":"prismatic"}),
        [1., 0., 0.],
        [0., 0., 0.],
        bodies,
    );
    world.validate_pose(&mechanism.evaluate().unwrap()).unwrap();
    assert!(!world.sweep(&mechanism, "drive", 0.08).unwrap().blocked);
    let bodies =
        json!({"mover":[box_piece([0.01,0.004,0.002])],"wall":[box_piece([0.1,0.01,0.01])]});
    let (mechanism, world) = fixture(
        json!({"type":"prismatic"}),
        [1., 0., 0.],
        [0., 0., 0.],
        bodies,
    );
    assert_eq!(
        world
            .validate_pose(&mechanism.evaluate().unwrap())
            .unwrap_err()
            .code(),
        "COLLISION_INITIAL_OVERLAP"
    );
}
#[test]
fn coverage_invalid_dimensions_and_unverified_or_omitted_bodies_are_not_accepted() {
    let mut definition = json!({"collision":{"bodies":{"mover":[box_piece([0.005;3])]}}});
    assert!(kinematics::decode(&definition).is_ok());
    definition["collision"]["bodies"]["mover"][0]["shape"]["half_extents_m"] = json!([0, 1, 1]);
    assert!(kinematics::decode(&definition).is_err());
    let definition =
        kinematics::decode(&json!({"collision":{"bodies":{"mover":[box_piece([0.005;3])]}}}))
            .unwrap()
            .collision
            .unwrap();
    let model = json!({"parts":[{"instance_id":"mover","position":[0,0,0]}]});
    assert!(World::new(definition.clone(), &model).is_err());
    let model = json!({"parts":[{"instance_id":"mover","position":[0,0,0]},{"instance_id":"omitted","position":[0,0,0]}]});
    assert!(World::new(definition, &model).is_err());
}

#[test]
fn ignored_pairs_stay_explicit_and_cancelled_sweeps_do_not_mutate_coordinates() {
    let bodies = json!({"mover":[box_piece([0.005;3])],"wall":[box_piece([0.0001,0.02,0.02])]});
    let (mechanism, world) = fixture(
        json!({"type":"prismatic"}),
        [1., 0., 0.],
        [50., 0., 0.],
        bodies,
    );
    let (_send, cancel) = tokio::sync::watch::channel(true);
    assert_eq!(
        world
            .sweep_cancel(&mechanism, "drive", 0.1, Some(&cancel))
            .unwrap_err()
            .code(),
        "CANCELLED"
    );
    assert_eq!(mechanism.evaluate().unwrap().joints[0].coordinate, 0.);
    let mut definition = world.definition().clone();
    definition.ignored_pairs = vec![["wall".into(), "mover".into()]];
    let model = json!({"parts":[{"instance_id":"mover","position":[0,0,0]},{"instance_id":"wall","position":[50,0,0]}],"collision_validation":{"method":"brep_boolean_coverage","covered_instances":["mover","wall"]}});
    let ignored = World::new(definition, &model).unwrap();
    assert!(!ignored.sweep(&mechanism, "drive", 0.1).unwrap().blocked);
    let report = ignored.report(&mechanism.evaluate().unwrap()).unwrap();
    assert!(report.checked_pairs.is_empty());
    assert_eq!(report.ignored_pairs.len(), 1);
    assert!(report.minimum_expanded_proxy_separation_m.is_none());
}

#[test]
fn a_touching_hinge_may_separate_but_a_later_reentry_in_the_same_turn_is_blocked() {
    let bodies = json!({"mover":[{"frame":{"position_m":[0.05,0,0]},"shape":{"type":"box","half_extents_m":[0.004,0.002,0.002]}}],"wall":[{"shape":{"type":"ball","radius_m":0.003}}]});
    let (mechanism, world) = fixture(
        json!({"type":"revolute"}),
        [0., 0., 1.],
        [50., 5., 0.],
        bodies,
    );
    let into = world.sweep(&mechanism, "drive", 0.5).unwrap();
    assert!(into.blocked);
    assert!(into.fraction < 1e-8, "{into:?}");
    assert!(!world.sweep(&mechanism, "drive", -0.5).unwrap().blocked);
    let turn = world
        .sweep(&mechanism, "drive", -std::f64::consts::TAU)
        .unwrap();
    assert!(turn.blocked, "{turn:?}");
    assert!(turn.fraction > 0.9 && turn.fraction < 1., "{turn:?}");
}

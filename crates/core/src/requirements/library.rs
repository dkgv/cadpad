//! The rule library (spec §4): the only place meaning comes from. A registry
//! of primitives (measurements) and rules (compiler macros over primitives),
//! each with a signature, documented defaults, a print template and the
//! contacts it covers. The whole registry is hashed into [`library_version`],
//! which is part of every result's inputs and of every expanded claim.
//!
//! Print templates: `{name}` is replaced by the printed argument; a segment in
//! `[...]` is printed only when every argument inside it is present.
use super::diagnostics::{Diagnostic, DiagnosticCode};
use super::ir::{AllowKind, Arg, Comparator, PartName, StateSpace, canonical};
use super::quantity::Dimension;
use crate::portable;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// What an argument must be. Entity kinds and single-vs-set are part of the
/// signature and are type-checked.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArgKind {
    /// Published parts: a part, glob, named set, `others` or a list of parts.
    /// `single` means one part per measurement: a set is applied part by part
    /// (see [`Pairing`](super::expand::Pairing)).
    Parts {
        single: bool,
    },
    /// Parts or features (faces/edges).
    Entities {
        single: bool,
    },
    Axis,
    /// One axis or a list of axes/directions (feature axes allowed).
    Axes,
    Joint,
    Quantity {
        dimension: Dimension,
    },
    /// Exactly `count` quantities in a list, all of one dimension.
    Quantities {
        dimension: Dimension,
        count: usize,
    },
    /// A comparator; its operands must have `dimension` when given.
    Compare {
        dimension: Option<Dimension>,
    },
    /// A state space, or a list of them (a union).
    States,
    /// A word with provenance; `allowed` empty means any.
    Token {
        allowed: Vec<&'static str>,
    },
    OneOf {
        kinds: Vec<ArgKind>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct ArgSpec {
    pub name: &'static str,
    pub kind: ArgKind,
    pub required: bool,
    pub doc: &'static str,
    /// Default used when the argument is omitted. These are library defaults
    /// (source `library`), printed in the expansion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Arg>,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Accuracy {
    /// OCC BRep query on a fresh build.
    #[default]
    Exact,
    /// Dense surface samples, printed with the spacing.
    Sampled,
    /// Rust dynamics over conservative collision proxies, never an exact CAD query.
    Proxy,
}

#[derive(Clone, Debug, Serialize)]
pub struct PrimitiveDef {
    pub name: &'static str,
    pub summary: &'static str,
    pub args: Vec<ArgSpec>,
    pub result: Dimension,
    pub accuracy: Accuracy,
    pub template: &'static str,
}

/// Contact a rule declares itself, so it never fails on it (spec §3.5).
/// `between` names two of the rule's arguments.
#[derive(Clone, Debug, Serialize)]
pub struct CoveredContact {
    pub between: [&'static str; 2],
    pub kind: AllowKind,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct RuleDef {
    pub name: &'static str,
    pub summary: &'static str,
    pub args: Vec<ArgSpec>,
    pub template: &'static str,
    pub covers: Vec<CoveredContact>,
    /// False when the signature is published but expansion needs primitives
    /// that do not exist yet (such claims fail with `E_TYPE_KIND`).
    pub available: bool,
    /// Project settings the expansion reads (part of the result's inputs).
    pub settings: Vec<&'static str>,
    /// Primitives the expansion can emit.
    pub expands_to: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Library {
    pub rules: Vec<RuleDef>,
    pub primitives: Vec<PrimitiveDef>,
}

fn spec(name: &'static str, kind: ArgKind, required: bool, doc: &'static str) -> ArgSpec {
    ArgSpec {
        name,
        kind,
        required,
        doc,
        default: None,
    }
}

fn parts(name: &'static str, single: bool, doc: &'static str) -> ArgSpec {
    spec(name, ArgKind::Parts { single }, true, doc)
}

fn at_arg(doc: &'static str, required: bool) -> ArgSpec {
    let mut a = spec("at", ArgKind::States, required, doc);
    if !required {
        a.default = Some(Arg::States(Box::new(StateSpace::Rest {})));
    }
    a
}

impl Library {
    fn build() -> Library {
        let len = Dimension::Length;
        let primitives = vec![
            PrimitiveDef {
                name: "joint",
                summary: "Mechanism joint coordinate in metres or radians, from a proxy simulation trace.",
                args: vec![spec("j", ArgKind::Joint, true, "the mechanism joint")],
                result: Dimension::Length,
                accuracy: Accuracy::Proxy,
                template: "coordinate of {j}",
            },
            PrimitiveDef {
                name: "displacement",
                summary: "Projected body displacement from the initial pose, from a proxy simulation trace.",
                args: vec![
                    parts("body", true, "the body"),
                    spec("direction", ArgKind::Axis, true, "world direction"),
                ],
                result: Dimension::Length,
                accuracy: Accuracy::Proxy,
                template: "displacement of {body} along {direction}",
            },
            PrimitiveDef {
                name: "contact",
                summary: "Whether a pair has a sampled contact impulse in the proxy simulation.",
                args: vec![
                    parts("a", true, "first body"),
                    parts("b", true, "second body"),
                ],
                result: Dimension::Dimensionless,
                accuracy: Accuracy::Proxy,
                template: "contact between {a} and {b}",
            },
            PrimitiveDef {
                name: "contact_impulse",
                summary: "Sampled contact impulse projected along a world direction for a body pair in the proxy simulation.",
                args: vec![
                    parts("a", true, "first body"),
                    parts("b", true, "second body"),
                    spec("direction", ArgKind::Axis, true, "world direction"),
                ],
                result: Dimension::Impulse,
                accuracy: Accuracy::Proxy,
                template: "contact impulse between {a} and {b}",
            },
            PrimitiveDef {
                name: "push",
                summary: "Additional receiver displacement with a driver joint enabled, versus the same proxy run with that driver disabled.",
                args: vec![
                    parts("pusher", true, "pushing body"),
                    parts("receiver", true, "receiving body"),
                    spec(
                        "driver_joint",
                        ArgKind::Joint,
                        true,
                        "joint motor to disable for the baseline",
                    ),
                    spec("direction", ArgKind::Axis, true, "world direction"),
                ],
                result: Dimension::Length,
                accuracy: Accuracy::Proxy,
                template: "additional displacement of {receiver} when {pusher} pushes with {driver_joint}",
            },
            PrimitiveDef {
                name: "interference",
                summary: "Volume of the intersection of two parts.",
                args: vec![
                    parts("a", true, "first part"),
                    parts("b", true, "second part"),
                ],
                result: Dimension::Volume,
                accuracy: Accuracy::Exact,
                template: "overlap volume of {a} and {b}",
            },
            PrimitiveDef {
                name: "contact_area",
                summary: "Area of common faces between two parts without volume overlap.",
                args: vec![
                    parts("a", true, "first part"),
                    parts("b", true, "second part"),
                ],
                result: Dimension::Area,
                accuracy: Accuracy::Exact,
                template: "area of common faces between {a} and {b}",
            },
            PrimitiveDef {
                name: "min_clearance",
                summary: "Smallest distance between two sets of entities (0 when touching).",
                args: vec![
                    spec("a", ArgKind::Entities { single: false }, true, "first set"),
                    spec("b", ArgKind::Entities { single: false }, true, "second set"),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "smallest distance between {a} and {b}",
            },
            // Spec §4.2 lists `distance(a, b)` as a name for this measurement.
            PrimitiveDef {
                name: "distance",
                summary: "Alias of `min_clearance`: smallest distance between two sets of entities (0 when touching).",
                args: vec![
                    spec("a", ArgKind::Entities { single: false }, true, "first set"),
                    spec("b", ArgKind::Entities { single: false }, true, "second set"),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "distance between {a} and {b}",
            },
            PrimitiveDef {
                name: "extent",
                summary: "Length of the world bounding box of a set along an axis.",
                args: vec![
                    spec("a", ArgKind::Parts { single: false }, true, "the set"),
                    spec("axis", ArgKind::Axis, true, "x, y, z or a vector"),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "extent of {a} along {axis}",
            },
            // P2.9–P2.13: measurements on bound features (faces/edges) as well
            // as whole parts. `Entities` arguments resolve to part keys plus an
            // optional feature descriptor the worker resolves per state.
            PrimitiveDef {
                name: "diameter",
                summary: "Diameter of a cylindrical face, circular edge or single-axis solid (twice the radius).",
                args: vec![spec(
                    "a",
                    ArgKind::Entities { single: true },
                    true,
                    "the feature or part",
                )],
                result: len,
                accuracy: Accuracy::Exact,
                template: "diameter of {a}",
            },
            PrimitiveDef {
                name: "radius",
                summary: "Radius of a cylindrical face, circular edge, spherical face or single-axis solid.",
                args: vec![spec(
                    "a",
                    ArgKind::Entities { single: true },
                    true,
                    "the feature or part",
                )],
                result: len,
                accuracy: Accuracy::Exact,
                template: "radius of {a}",
            },
            PrimitiveDef {
                name: "length",
                summary: "Length of an edge (exact curve length), a face or a part (largest bounding-box dimension); a set reports its longest member.",
                args: vec![spec(
                    "a",
                    ArgKind::Entities { single: false },
                    true,
                    "the feature(s) or part(s)",
                )],
                result: len,
                accuracy: Accuracy::Exact,
                template: "length of {a}",
            },
            PrimitiveDef {
                name: "area",
                summary: "Surface area of faces or whole parts; a set reports the total.",
                args: vec![spec(
                    "a",
                    ArgKind::Entities { single: false },
                    true,
                    "the feature(s) or part(s)",
                )],
                result: Dimension::Area,
                accuracy: Accuracy::Exact,
                template: "area of {a}",
            },
            PrimitiveDef {
                name: "volume",
                summary: "Volume of solids; a set reports the total.",
                args: vec![spec(
                    "a",
                    ArgKind::Entities { single: false },
                    true,
                    "the feature(s) or part(s)",
                )],
                result: Dimension::Volume,
                accuracy: Accuracy::Exact,
                template: "volume of {a}",
            },
            PrimitiveDef {
                name: "gap",
                summary: "Gap between two features: the exact minimum distance, and a maximum sampled over the surfaces every `spacing` (default derived from the bounding box). `in [lo, hi]` requires min >= lo and max <= hi; `<=` bounds the max, `>=` bounds the min.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "first feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "second feature or part",
                    ),
                    spec(
                        "spacing",
                        ArgKind::Quantity { dimension: len },
                        false,
                        "surface sampling step for the maximum",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Sampled,
                template: "gap between {a} and {b}[, sampled every {spacing}]",
            },
            PrimitiveDef {
                name: "clearance_along",
                summary: "Clearance of A from B along each direction: the distance A travels purely along the direction before B is reached ahead of it (shadow clearance; perpendicular offsets are ignored, so it never overstates). The result is the smallest over `dirs` and over the entities of each side, so a feature selection with several faces reports the tightest wall.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: false },
                        true,
                        "the moving feature(s) or part(s)",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: false },
                        true,
                        "the obstacle feature(s) or part(s)",
                    ),
                    spec(
                        "directions",
                        ArgKind::Axes,
                        true,
                        "directions to check, e.g. [+x, -x, +y, -y]",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "clearance of {a} from {b} along {directions}",
            },
            PrimitiveDef {
                name: "overlap_depth",
                summary: "Depth of the volumetric overlap of A and B measured along `along`: the extent of the intersection projected on the direction (0 when they do not overlap). A set reports the deepest pair.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "first feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "second feature or part",
                    ),
                    spec(
                        "along",
                        ArgKind::OneOf {
                            kinds: vec![ArgKind::Axis, ArgKind::Entities { single: true }],
                        },
                        true,
                        "the direction to measure along (axis, direction word or feature)",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "overlap depth of {a} and {b} along {along}",
            },
            PrimitiveDef {
                name: "axis_offset",
                summary: "Distance between the axes of two features (cylinder axis, circle or line edge, plane normal). A set reports the largest offset. Fails when a side has no single axis.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "first feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "second feature or part",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "offset between the axes of {a} and {b}",
            },
            PrimitiveDef {
                name: "axis_angle",
                summary: "Acute angle between the axes of two features, in radians (0 when parallel or anti-parallel). A set reports the largest angle.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "first feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "second feature or part",
                    ),
                ],
                result: Dimension::Angle,
                accuracy: Accuracy::Exact,
                template: "angle between the axes of {a} and {b}",
            },
            PrimitiveDef {
                name: "diameter_difference",
                summary: "Diameter of a minus diameter of b (negative when b is larger); a set reports the pair with the largest absolute difference.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "first feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "second feature or part",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "diameter of {a} minus diameter of {b}",
            },
            PrimitiveDef {
                name: "thread_protrusion",
                summary: "Length of a fastener's threaded end protruding past the far side of the parts it mates (fastener metadata `mates`), measured along the fastener axis; 0 when it does not reach through. The head end is recognised by its larger profile.",
                args: vec![spec(
                    "a",
                    ArgKind::Entities { single: true },
                    true,
                    "the fastener part",
                )],
                result: len,
                accuracy: Accuracy::Exact,
                template: "thread protrusion of {a}",
            },
            PrimitiveDef {
                name: "profile_centre",
                summary: "Distance between the centres of the bounding profiles of two features perpendicular to `axis` (0 when centred). A set reports the largest offset.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "reference feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "feature or part to compare",
                    ),
                    spec(
                        "axis",
                        ArgKind::OneOf {
                            kinds: vec![ArgKind::Axis, ArgKind::Entities { single: true }],
                        },
                        true,
                        "profile normal (x, y, z, vector, direction word or feature)",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "centre offset of {b} from {a} across {axis}",
            },
            PrimitiveDef {
                name: "profile_size",
                summary: "How much larger the bounding profile of b is than a's perpendicular to `axis`: the difference of the larger in-plane bounding-box extents. A set reports the smallest difference.",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "reference feature or part",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "feature or part to compare",
                    ),
                    spec(
                        "axis",
                        ArgKind::OneOf {
                            kinds: vec![ArgKind::Axis, ArgKind::Entities { single: true }],
                        },
                        true,
                        "profile normal (x, y, z, vector, direction word or feature)",
                    ),
                ],
                result: len,
                accuracy: Accuracy::Exact,
                template: "profile size of {b} minus {a} across {axis}",
            },
            // Not in the spec's §4.2 table; the v1 rules `inside` and
            // `single_solid` need them. Flagged in the report.
            PrimitiveDef {
                name: "outside_volume",
                summary: "Volume of a part lying outside an envelope.",
                args: vec![
                    parts("a", true, "the part"),
                    parts("envelope", false, "envelope part(s)"),
                ],
                result: Dimension::Volume,
                accuracy: Accuracy::Exact,
                template: "volume of {a} outside {envelope}",
            },
            PrimitiveDef {
                name: "valid_solid_count",
                summary: "Number of valid closed solids in a part.",
                args: vec![parts("a", true, "the part")],
                result: Dimension::Dimensionless,
                accuracy: Accuracy::Exact,
                template: "number of valid solids in {a}",
            },
        ];
        let eps = vec!["overlap_tolerance"];
        let rules = vec![
            RuleDef {
                name: "no_overlap",
                summary: "No part of A overlaps any part of B beyond ε, at every state.",
                args: vec![
                    parts("a", false, "first group"),
                    parts("b", false, "second group"),
                    at_arg("states to check", false),
                ],
                template: "{a} does not overlap {b}, {at}",
                covers: vec![],
                available: true,
                settings: eps.clone(),
                expands_to: vec!["interference"],
            },
            RuleDef {
                name: "moves_freely",
                summary: "Moving parts clear the others over a state space; optional minimum clearance.",
                args: vec![
                    parts("moving", false, "the moving set"),
                    parts("against", false, "parts it must clear, or `others`"),
                    at_arg_named("over", "state space the motion covers"),
                    spec(
                        "clearance",
                        ArgKind::Compare {
                            dimension: Some(len),
                        },
                        false,
                        "minimum clearance, e.g. >= 0.5 mm",
                    ),
                ],
                template: "{moving} does not overlap {against}, {over}[; clearance {clearance}]",
                covers: vec![],
                available: true,
                settings: eps.clone(),
                expands_to: vec!["interference", "min_clearance"],
            },
            RuleDef {
                name: "blocked",
                summary: "Moving parts are stopped by a part: some pair overlaps beyond ε at every listed state (hard stops, keys).",
                args: vec![
                    parts("moving", false, "the moving set"),
                    parts("by", false, "the stopping part(s)"),
                    at_arg("states where it is blocked", true),
                ],
                template: "{moving} is blocked by {by}, {at}",
                covers: vec![],
                available: true,
                settings: eps.clone(),
                expands_to: vec!["interference"],
            },
            RuleDef {
                name: "inside",
                summary: "A lies fully inside an envelope.",
                args: vec![
                    parts("a", false, "the part(s)"),
                    parts("envelope", false, "envelope part(s)"),
                    at_arg("states to check", false),
                ],
                template: "{a} lies inside {envelope}[, {at}]",
                covers: vec![],
                available: true,
                settings: eps.clone(),
                expands_to: vec!["outside_volume"],
            },
            RuleDef {
                name: "clear_of",
                summary: "A lies fully outside an envelope.",
                args: vec![
                    parts("a", false, "the part(s)"),
                    parts("envelope", false, "envelope part(s)"),
                    at_arg("states to check", false),
                ],
                template: "{a} is clear of {envelope}[, {at}]",
                covers: vec![],
                available: true,
                settings: eps.clone(),
                expands_to: vec!["interference"],
            },
            RuleDef {
                name: "fits_bed",
                summary: "Extents fit the bed in the stated orientation (no orientation search).",
                args: vec![
                    parts("a", false, "the part(s)"),
                    spec(
                        "bed",
                        ArgKind::Quantities {
                            dimension: len,
                            count: 3,
                        },
                        true,
                        "bed size [x, y, z]: three lengths from the text, the model or an inferred proposal; never a library default",
                    ),
                    spec(
                        "orientation",
                        ArgKind::Token {
                            allowed: vec!["as_modelled"],
                        },
                        true,
                        "only `as_modelled` (model axes = bed axes) in v1",
                    ),
                ],
                template: "{a} fits within {bed}, {orientation}",
                covers: vec![],
                available: true,
                settings: vec![],
                expands_to: vec!["extent"],
            },
            RuleDef {
                name: "bolted",
                summary: "P1 is bolted to P2 by the fasteners: coaxial holes, hole diameter, shank spans the stack, nut engaged, parts bear on each other. The three limits are arguments, never library values.",
                args: vec![
                    parts("p1", false, "first bolted part(s)"),
                    parts("p2", false, "second bolted part(s)"),
                    parts("fasteners", false, "bolts and nuts"),
                    spec(
                        "size",
                        ArgKind::Token { allowed: vec![] },
                        true,
                        "thread size as written, e.g. M8",
                    ),
                    spec(
                        "hole",
                        ArgKind::Compare {
                            dimension: Some(len),
                        },
                        true,
                        "hole diameter limit, e.g. in [8.4 mm, 9 mm]",
                    ),
                    spec(
                        "protrusion",
                        ArgKind::Compare {
                            dimension: Some(len),
                        },
                        true,
                        "thread protrusion limit, e.g. >= 1 mm",
                    ),
                    spec(
                        "bearing",
                        ArgKind::Compare {
                            dimension: Some(Dimension::Area),
                        },
                        true,
                        "contact area between the parts, e.g. >= 100 mm2",
                    ),
                ],
                template: "{p1} is bolted to {p2} with {fasteners} ({size}); hole {hole}, protrusion {protrusion}, bearing {bearing}",
                covers: vec![
                    CoveredContact {
                        between: ["fasteners", "p1"],
                        kind: AllowKind::Touch,
                        reason: "fastener in its hole",
                    },
                    CoveredContact {
                        between: ["fasteners", "p2"],
                        kind: AllowKind::Touch,
                        reason: "fastener in its hole",
                    },
                ],
                available: true,
                settings: eps.clone(),
                expands_to: vec![
                    "diameter",
                    "interference",
                    "contact_area",
                    "thread_protrusion",
                ],
            },
            RuleDef {
                name: "aligned",
                summary: "A's bounding profile is centred on B's within `tol` and B's opening exceeds A's by `margin`, measured perpendicular to `plane` (or to B's own axis/normal when `plane` is omitted).",
                args: vec![
                    spec(
                        "a",
                        ArgKind::Entities { single: true },
                        true,
                        "feature or part to centre",
                    ),
                    spec(
                        "b",
                        ArgKind::Entities { single: true },
                        true,
                        "reference feature or part (its opening)",
                    ),
                    spec(
                        "plane",
                        ArgKind::OneOf {
                            kinds: vec![ArgKind::Axis, ArgKind::Entities { single: true }],
                        },
                        false,
                        "profile normal (axis, direction word or feature); defaults to B's own axis",
                    ),
                    spec(
                        "tol",
                        ArgKind::Quantity { dimension: len },
                        true,
                        "allowed centre offset, e.g. 0.2 mm",
                    ),
                    spec(
                        "margin",
                        ArgKind::Quantity { dimension: len },
                        true,
                        "how much B's opening must exceed A's, e.g. 0.3 mm",
                    ),
                    at_arg("states to check", false),
                ],
                template: "{a} is aligned with {b} within {tol} and clears it by {margin}[, {at}]",
                covers: vec![],
                available: true,
                settings: vec![],
                expands_to: vec!["profile_centre", "profile_size"],
            },
            RuleDef {
                name: "press_fit",
                summary: "diameter(pin) - diameter(hole) within `range` (positive = interference) and the axes coaxial within half the minimum guaranteed radial engagement.",
                args: vec![
                    spec(
                        "pin",
                        ArgKind::Entities { single: true },
                        true,
                        "the pin feature or part",
                    ),
                    spec(
                        "hole",
                        ArgKind::Entities { single: true },
                        true,
                        "the hole feature or part",
                    ),
                    spec(
                        "range",
                        ArgKind::Compare {
                            dimension: Some(len),
                        },
                        true,
                        "diametral difference limit, e.g. in [0.02 mm, 0.05 mm]",
                    ),
                    at_arg("states to check", false),
                ],
                template: "{pin} is a press fit in {hole}: diameter difference {range}[, {at}]",
                covers: vec![],
                available: true,
                settings: vec![],
                expands_to: vec!["diameter_difference", "axis_offset"],
            },
            RuleDef {
                name: "fit",
                summary: "A feature's diameter lies inside the ISO 286 tolerance zone of `class` at `nominal` (e.g. 8 mm H7 = 8.000–8.015 mm).",
                args: vec![
                    spec(
                        "feature",
                        ArgKind::Entities { single: true },
                        true,
                        "the bore or shaft feature",
                    ),
                    spec(
                        "class",
                        ArgKind::Token { allowed: vec![] },
                        true,
                        "ISO 286 class as written, e.g. H7, h6, js6",
                    ),
                    spec(
                        "nominal",
                        ArgKind::OneOf {
                            kinds: vec![
                                ArgKind::Quantity { dimension: len },
                                ArgKind::Token { allowed: vec![] },
                            ],
                        },
                        true,
                        "nominal diameter, e.g. 8 mm, or a fastener size like M8",
                    ),
                    at_arg("states to check", false),
                ],
                template: "{feature} is {class} at {nominal}[, {at}]",
                covers: vec![],
                available: true,
                settings: vec![],
                expands_to: vec!["diameter"],
            },
            RuleDef {
                name: "seated",
                summary: "A rests on B: no overlap beyond ε and a positive common-face contact area (at least `area` when given).",
                args: vec![
                    parts("a", false, "the seated part(s)"),
                    parts("on", false, "the part(s) they rest on"),
                    spec(
                        "area",
                        ArgKind::Quantity {
                            dimension: Dimension::Area,
                        },
                        false,
                        "minimum contact area, e.g. 100 mm2; any contact when omitted",
                    ),
                    at_arg("states to check", false),
                ],
                template: "{a} is seated on {on}[ with contact area {area}][, {at}]",
                covers: vec![],
                available: true,
                settings: eps.clone(),
                expands_to: vec!["interference", "contact_area"],
            },
            RuleDef {
                name: "single_solid",
                summary: "Each part is one valid closed solid.",
                args: vec![parts("a", false, "the part(s)")],
                template: "{a} is one valid solid",
                covers: vec![],
                available: true,
                settings: vec![],
                expands_to: vec!["valid_solid_count"],
            },
        ];
        Library { rules, primitives }
    }

    pub fn rule(&self, name: &str) -> Option<&RuleDef> {
        self.rules.iter().find(|r| r.name == name)
    }
    pub fn primitive(&self, name: &str) -> Option<&PrimitiveDef> {
        self.primitives.iter().find(|p| p.name == name)
    }
    /// SHA-256 over canonical rule registry and standards reference data.
    pub fn version(&self) -> String {
        self.version_with_reference_data(
            &super::iso286::reference_data(),
            &super::fasteners::reference_data(),
        )
    }

    fn version_with_reference_data(
        &self,
        iso286: &serde_json::Value,
        fasteners: &serde_json::Value,
    ) -> String {
        let value = serde_json::to_value(self).expect("library serializes");
        let mut text = String::new();
        canonical(&value, &mut text);
        let registry = portable::bytes_hash(text.as_bytes());
        let value =
            serde_json::json!({ "registry": registry, "iso286": iso286, "fasteners": fasteners });
        let mut text = String::new();
        canonical(&value, &mut text);
        portable::bytes_hash(text.as_bytes())
    }
}

/// Signature of the optional `at` states argument of a `measure` claim.
pub fn at_spec() -> ArgSpec {
    at_arg("states to check", false)
}

fn at_arg_named(name: &'static str, doc: &'static str) -> ArgSpec {
    spec(name, ArgKind::States, true, doc)
}

/// The v1 library.
pub fn library() -> &'static Library {
    static LIBRARY: OnceLock<Library> = OnceLock::new();
    LIBRARY.get_or_init(Library::build)
}

/// Version of the rules and reference datasets. Changes mark requirements
/// using them `stale`.
pub fn library_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| library().version())
}

// ---------------------------------------------------------------- signatures

fn is_parts_like(arg: &Arg) -> bool {
    match arg {
        Arg::Part(_) | Arg::Set(_) | Arg::Others => true,
        Arg::List(items) => !items.is_empty() && items.iter().all(|i| matches!(i, Arg::Part(_))),
        _ => false,
    }
}

/// Parts, features or lists mixing them (only meaningful for
/// [`ArgKind::Entities`]; `Parts` keeps the stricter part-only check).
fn is_entities_like(arg: &Arg) -> bool {
    match arg {
        Arg::Feature(_) => true,
        Arg::List(items) => !items.is_empty() && items.iter().all(is_entities_like),
        _ => is_parts_like(arg),
    }
}

/// A direction keyword arg: `"+x"`, `"-y"`, … (see `Arg::Text`).
fn is_direction_word(arg: &Arg) -> bool {
    matches!(arg, Arg::Text(t) if matches!(t.as_str(), "+x" | "-x" | "+y" | "-y" | "+z" | "-z"))
}

fn describe(arg: &Arg) -> &'static str {
    match arg {
        Arg::Model(_) => "a model",
        Arg::Part(_) => "a part",
        Arg::Set(_) => "a set",
        Arg::Others => "`others`",
        Arg::Param(_) => "a parameter",
        Arg::Joint(_) => "a joint",
        Arg::Feature(_) => "a feature",
        Arg::Axis(_) => "an axis",
        Arg::Quantity(_) => "a quantity",
        Arg::Number(_) => "a number",
        Arg::Text(_) => "a keyword",
        Arg::Token(_) => "a token",
        Arg::Bool(_) => "a boolean",
        Arg::Compare(_) => "a comparison",
        Arg::States(_) => "a state space",
        Arg::List(_) => "a list",
    }
}

fn kind_name(kind: &ArgKind) -> String {
    match kind {
        ArgKind::Parts { single: true } => "a single part".into(),
        ArgKind::Parts { single: false } => "parts (a part, glob, set, `others` or list)".into(),
        ArgKind::Entities { single: true } => "a single part or feature".into(),
        ArgKind::Entities { single: false } => "parts or features".into(),
        ArgKind::Axis => "an axis".into(),
        ArgKind::Axes => "an axis or a list of axes".into(),
        ArgKind::Joint => "a mechanism joint".into(),
        ArgKind::Quantity { dimension } => format!("a {}", dimension.name()),
        ArgKind::Quantities { dimension, count } => {
            format!("a list of {count} {} values", dimension.name())
        }
        ArgKind::Compare { .. } => "a comparison".into(),
        ArgKind::States => "a state space".into(),
        ArgKind::Token { allowed } if allowed.is_empty() => "a token".into(),
        ArgKind::Token { allowed } => format!("one of {}", allowed.join(", ")),
        ArgKind::OneOf { kinds } => kinds.iter().map(kind_name).collect::<Vec<_>>().join(" or "),
    }
}

/// Check one argument against its kind. `relax_single`: a `single` part
/// argument also accepts a set (primitives applied through pairing).
fn check_kind(kind: &ArgKind, arg: &Arg, at: &str, relax_single: bool, out: &mut Vec<Diagnostic>) {
    let mismatch = || {
        Diagnostic::new(
            DiagnosticCode::ETypeKind,
            at,
            format!("expected {} but found {}", kind_name(kind), describe(arg)),
        )
    };
    match kind {
        ArgKind::Parts { single } | ArgKind::Entities { single } => {
            let is_feature = matches!(kind, ArgKind::Entities { .. }) && is_entities_like(arg);
            if !is_parts_like(arg) && !is_feature {
                out.push(mismatch());
            } else if *single && !relax_single {
                let one = match arg {
                    Arg::Part(p) => !matches!(p.part, PartName::Glob(_)),
                    Arg::Feature(_) => true,
                    _ => false,
                };
                if !one {
                    out.push(mismatch());
                }
            }
        }
        ArgKind::Axis => {
            if !matches!(arg, Arg::Axis(_)) && !is_direction_word(arg) {
                out.push(mismatch());
            }
        }
        ArgKind::Axes => match arg {
            Arg::Axis(_) => {}
            _ if is_direction_word(arg) => {}
            Arg::List(items)
                if !items.is_empty()
                    && items
                        .iter()
                        .all(|i| matches!(i, Arg::Axis(_)) || is_direction_word(i)) => {}
            _ => out.push(mismatch()),
        },
        ArgKind::Joint => {
            if !matches!(arg, Arg::Joint(_)) {
                out.push(mismatch());
            }
        }
        ArgKind::Quantity { dimension } => match arg {
            Arg::Quantity(l) => {
                if let Err(e) = l.quantity().expect(*dimension) {
                    out.push(e.diagnostic(at));
                }
            }
            _ => out.push(mismatch()),
        },
        ArgKind::Quantities { dimension, count } => match arg {
            Arg::List(items) if items.len() == *count => {
                for (i, item) in items.iter().enumerate() {
                    check_kind(
                        &ArgKind::Quantity {
                            dimension: *dimension,
                        },
                        item,
                        &format!("{at}[{i}]"),
                        relax_single,
                        out,
                    );
                }
            }
            _ => out.push(mismatch()),
        },
        ArgKind::Compare { dimension } => match arg {
            Arg::Compare(c) => {
                out.extend(c.validate(at));
                if let (Some(want), Some(have)) = (dimension, c.dimension())
                    && want != &have
                {
                    out.push(
                        super::quantity::DimensionMismatch {
                            expected: *want,
                            found: have,
                        }
                        .diagnostic(at),
                    );
                }
                if matches!(c, Comparator::IsTrue {} | Comparator::IsFalse {})
                    && dimension.is_some()
                {
                    out.push(mismatch());
                }
            }
            _ => out.push(mismatch()),
        },
        ArgKind::States => {
            let validate =
                |s: &StateSpace, p: String, out: &mut Vec<Diagnostic>| out.extend(s.validate(&p));
            match arg {
                Arg::States(s) => validate(s, at.to_string(), out),
                Arg::List(items)
                    if !items.is_empty() && items.iter().all(|i| matches!(i, Arg::States(_))) =>
                {
                    for (i, item) in items.iter().enumerate() {
                        if let Arg::States(s) = item {
                            validate(s, format!("{at}[{i}]"), out);
                        }
                    }
                }
                _ => out.push(mismatch()),
            }
        }
        ArgKind::Token { allowed } => match arg {
            Arg::Token(t) if allowed.is_empty() || allowed.contains(&t.text.as_str()) => {}
            _ => out.push(mismatch()),
        },
        ArgKind::OneOf { kinds } => {
            let ok = kinds.iter().any(|k| {
                let mut scratch = Vec::new();
                check_kind(k, arg, at, relax_single, &mut scratch);
                scratch.is_empty()
            });
            if !ok {
                out.push(mismatch());
            }
        }
    }
}

/// Check claim args against a signature: unknown keys, missing required
/// arguments, kinds, units. `ignore` lists keys the caller handles itself.
pub fn check_args(
    specs: &[ArgSpec],
    args: &BTreeMap<String, Arg>,
    at: &str,
    ignore: &[&str],
    relax_single: bool,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for name in args.keys() {
        if !ignore.contains(&name.as_str()) && !specs.iter().any(|s| s.name == name) {
            out.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                format!("{at}.args.{name}"),
                format!("unknown argument {name:?}"),
            ));
        }
    }
    for s in specs {
        match args.get(s.name) {
            Some(arg) => check_kind(
                &s.kind,
                arg,
                &format!("{at}.args.{}", s.name),
                relax_single,
                &mut out,
            ),
            None if s.required => out.push(Diagnostic::new(
                DiagnosticCode::ETypeKind,
                format!("{at}.args"),
                format!("missing required argument {:?}", s.name),
            )),
            None => {}
        }
    }
    out
}

// ---------------------------------------------------------------- vague words

/// One way a vague word can be read.
#[derive(Clone, Debug, Serialize)]
pub struct Reading {
    pub rules: Vec<&'static str>,
    pub summary: &'static str,
    /// Settings the reading depends on, if any.
    pub settings: Vec<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VagueEntry {
    /// Lower-case word or phrase.
    pub phrase: &'static str,
    pub readings: Vec<Reading>,
    /// Question for the user when more than one reading is plausible.
    pub question: &'static str,
}

impl VagueEntry {
    /// More than one reading: must become a question, never a guess.
    pub fn is_ambiguous(&self) -> bool {
        self.readings.len() > 1
    }
}

fn reading(rules: &[&'static str], summary: &'static str) -> Reading {
    Reading {
        rules: rules.to_vec(),
        summary,
        settings: vec![],
    }
}

/// Word to candidate rules. Entries with one reading are mappings ("clashing"
/// means overlap); entries with several are `E_VAGUE` questions. Every rule
/// named here exists in the v1 library (tested).
pub fn vague_table() -> &'static [VagueEntry] {
    static TABLE: OnceLock<Vec<VagueEntry>> = OnceLock::new();
    TABLE.get_or_init(|| {
        vec![
            VagueEntry {
                phrase: "freely",
                readings: vec![
                    reading(&["moves_freely"], "no overlap anywhere in the motion"),
                    reading(&["moves_freely"], "no overlap and a minimum gap (needs a clearance)"),
                ],
                question: "\"Freely\": no overlap, or a minimum gap as well? If a gap, how big?",
            },
            VagueEntry {
                phrase: "clashing",
                readings: vec![reading(&["no_overlap", "moves_freely"], "no overlap beyond the overlap tolerance")],
                question: "",
            },
            VagueEntry {
                phrase: "hitting",
                readings: vec![reading(&["no_overlap", "moves_freely"], "no overlap beyond the overlap tolerance")],
                question: "",
            },
            VagueEntry {
                phrase: "colliding",
                readings: vec![reading(&["no_overlap", "moves_freely"], "no overlap beyond the overlap tolerance")],
                question: "",
            },
            VagueEntry {
                phrase: "fits",
                readings: vec![
                    reading(&["inside"], "lies inside an envelope or enclosure"),
                    reading(&["fits_bed"], "extents fit a print bed"),
                    reading(&["no_overlap"], "assembles without overlapping the other part"),
                ],
                question: "\"Fits\": inside something, on a print bed, or just not overlapping?",
            },
            VagueEntry {
                phrase: "retractable",
                readings: vec![
                    reading(&["moves_freely", "clear_of"], "moves over its range and ends clear of a region"),
                    reading(&["moves_freely", "inside"], "moves over its range and ends inside an envelope"),
                ],
                question: "Retractable to what end? E.g. clear of something for loading, or inside an envelope.",
            },
            VagueEntry {
                phrase: "opens",
                readings: vec![
                    reading(&["moves_freely"], "moves to the stated limit without hitting"),
                    reading(&["moves_freely", "blocked"], "moves to the limit and is stopped there"),
                ],
                question: "Opens to the limit without hitting only, or should it also stop at the limit?",
            },
            VagueEntry {
                phrase: "printable",
                readings: vec![
                    reading(&["fits_bed"], "fits the bed"),
                    reading(&["single_solid"], "is one valid solid"),
                    reading(&["fits_bed", "single_solid"], "both"),
                ],
                question: "\"Printable\": fits the bed, is one solid, or both?",
            },
            VagueEntry {
                phrase: "clear of",
                readings: vec![reading(&["clear_of"], "lies fully outside the region")],
                question: "",
            },
            VagueEntry {
                phrase: "one solid",
                readings: vec![reading(&["single_solid"], "one valid closed solid")],
                question: "",
            },
        ]
    })
}

#[derive(Clone, Debug, Serialize)]
pub struct VagueMatch {
    /// Character span in the searched text.
    pub span: [usize; 2],
    pub phrase: &'static str,
    pub ambiguous: bool,
}

/// Find table words in prose (case-insensitive, whole words), in order.
pub fn find_vague(text: &str) -> Vec<VagueMatch> {
    let chars: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();
    // Lower-casing can change length for exotic characters; fall back to
    // original-length matching when it does.
    let chars = if chars.len() == text.chars().count() {
        chars
    } else {
        text.chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect()
    };
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for entry in vague_table() {
        let p: Vec<char> = entry.phrase.chars().collect();
        if chars.len() < p.len() {
            continue;
        }
        for s in 0..=chars.len() - p.len() {
            if chars[s..s + p.len()] == p[..]
                && (s == 0 || !is_word(chars[s - 1]))
                && chars.get(s + p.len()).is_none_or(|c| !is_word(*c))
            {
                out.push(VagueMatch {
                    span: [s, s + p.len()],
                    phrase: entry.phrase,
                    ambiguous: entry.is_ambiguous(),
                });
            }
        }
    }
    out.sort_by_key(|m| m.span);
    out
}

/// Placeholder names in a template (`{a}` inside or outside `[...]`).
pub fn template_args(template: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(s) = rest.find('{') {
        let Some(e) = rest[s..].find('}') else { break };
        out.push(&rest[s + 1..s + e]);
        rest = &rest[s + e + 1..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let lib = library();
        let names: std::collections::HashSet<_> = lib
            .rules
            .iter()
            .map(|r| r.name)
            .chain(lib.primitives.iter().map(|p| p.name))
            .collect();
        assert_eq!(
            names.len(),
            lib.rules.len() + lib.primitives.len(),
            "unique names"
        );
        for r in &lib.rules {
            let arg_names: Vec<_> = r.args.iter().map(|a| a.name).collect();
            for t in template_args(r.template) {
                assert!(
                    arg_names.contains(&t),
                    "{}: template uses unknown {t}",
                    r.name
                );
            }
            for a in &r.args {
                assert!(
                    a.required
                        || a.default.is_some()
                        || ["clearance", "plane", "area"].contains(&a.name),
                    "{}.{} optional without default",
                    r.name,
                    a.name
                );
            }
            for p in &r.expands_to {
                assert!(
                    lib.primitive(p).is_some(),
                    "{}: unknown primitive {p}",
                    r.name
                );
            }
            for c in &r.covers {
                assert!(c.between.iter().all(|n| arg_names.contains(n)));
            }
            for s in &r.settings {
                assert!(["overlap_tolerance"].contains(s), "{s}");
            }
        }
        for p in &lib.primitives {
            let arg_names: Vec<_> = p.args.iter().map(|a| a.name).collect();
            for t in template_args(p.template) {
                assert!(
                    arg_names.contains(&t),
                    "{}: template uses unknown {t}",
                    p.name
                );
            }
        }
        for want in [
            "no_overlap",
            "moves_freely",
            "blocked",
            "inside",
            "clear_of",
            "fits_bed",
            "single_solid",
            "bolted",
        ] {
            assert!(lib.rule(want).is_some(), "{want}");
        }
        for want in ["interference", "contact_area", "min_clearance", "extent"] {
            assert!(lib.primitive(want).is_some(), "{want}");
        }
    }

    #[test]
    fn rules_carry_no_design_values() {
        // v3 §4: clearance class, bearing area, thread protrusion and bed size
        // are arguments, never library defaults.
        let lib = library();
        let bolted = lib.rule("bolted").unwrap();
        assert!(bolted.available, "P3.6 expansion is implemented");
        for arg in ["size", "hole", "protrusion", "bearing"] {
            let a = bolted.args.iter().find(|a| a.name == arg).unwrap();
            assert!(a.required && a.default.is_none(), "{arg}");
        }
        for r in &lib.rules {
            for a in &r.args {
                // The only defaults are state spaces (`at` = as built).
                if let Some(d) = &a.default {
                    assert!(
                        matches!(d, Arg::States(_)),
                        "{}.{} default {d:?}",
                        r.name,
                        a.name
                    );
                }
            }
        }
        let fits = lib.rule("fits_bed").unwrap();
        let bed = fits.args.iter().find(|a| a.name == "bed").unwrap();
        assert!(matches!(bed.kind, ArgKind::Quantities { count: 3, .. }));
        assert!(!serde_json::to_string(lib).unwrap().contains("preset"));
    }

    #[test]
    fn version_is_a_stable_content_hash() {
        let v = library_version();
        assert_eq!(v.len(), 64);
        assert_eq!(v, library().version());
        let mut changed = Library::build();
        changed.rules[0].template = "changed";
        assert_ne!(changed.version(), v);
        let mut changed = Library::build();
        changed.primitives[0].accuracy = Accuracy::Sampled;
        assert_ne!(changed.version(), v);
        let mut changed = Library::build();
        changed.rules[1].args[0].required = false;
        assert_ne!(changed.version(), v);
        let base_iso = super::super::iso286::reference_data();
        let base_fasteners = super::super::fasteners::reference_data();
        assert_eq!(
            library().version_with_reference_data(&base_iso, &base_fasteners),
            v
        );
        let mut iso = base_iso.clone();
        iso["it_grades_um"][2][2] = serde_json::json!(15.1);
        assert_ne!(
            library().version_with_reference_data(&iso, &base_fasteners),
            v
        );
        let mut fasteners = base_fasteners.clone();
        fasteners["pitch_mm"][4][1] = serde_json::json!(1.26);
        assert_ne!(
            library().version_with_reference_data(&base_iso, &fasteners),
            v
        );
    }

    #[test]
    fn vague_table_only_names_library_rules() {
        for e in vague_table() {
            assert!(!e.readings.is_empty());
            assert_eq!(e.is_ambiguous(), !e.question.is_empty(), "{}", e.phrase);
            for r in &e.readings {
                for rule in &r.rules {
                    assert!(library().rule(rule).is_some(), "{} -> {rule}", e.phrase);
                }
            }
        }
    }

    #[test]
    fn vague_words_are_found_with_spans() {
        let m = find_vague("ensure turntable can rotate freely without clashing with base");
        let found: Vec<_> = m.iter().map(|m| (m.phrase, m.span, m.ambiguous)).collect();
        assert_eq!(
            found,
            [("freely", [28, 34], true), ("clashing", [43, 51], false)]
        );
        let m = find_vague("It FITS the bed, not unfits or fitsx");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].phrase, "fits");
        assert!(
            find_vague("turntable retractable?")
                .iter()
                .any(|m| m.phrase == "retractable" && m.ambiguous)
        );
    }
}

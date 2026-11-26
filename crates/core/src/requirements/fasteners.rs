//! Standard fastener dimension tables (ISO metric bolts, nuts, clearance
//! holes) as library data for the requirements engine.
//!
//! These are *reference tables*, not inferred geometry: per
//! `docs/requirements-spec.md` §4, fastener metadata published via
//! `show(..., fastener=dict(...))` is explicit, and a size label alone never
//! implies standard dimensions. Callers (P3.4 `thread_protrusion`, P3.6
//! `bolted`, …) decide when consulting a table is legitimate — e.g. to
//! resolve a nominal hole size or a nut height for a *declared* `size`.
//!
//! Citations:
//! - [`BoltStyle::SocketHeadCapScrew`] — ISO 4762:2012 (hexagon socket head
//!   cap screws, fully threaded, coarse pitch).
//! - [`BoltStyle::HexHeadBolt`] — ISO 4014:2011 (hexagon head bolts with
//!   shank, coarse pitch; supersedes DIN 931).
//! - [`NutDims`] — ISO 4032:2023 (hexagon regular nuts, style 1).
//! - [`ClearanceHole`] — ISO 273:1991 (clearance holes for bolts and
//!   screws, fine/medium/coarse series with H12/H13/H14 hole tolerances).
//!
//! All dimensions are nominal values in millimetres. Only coarse-pitch
//! threads are tabulated, matching what `size = "M8"` labels denote.

/// Coarse metric thread pitch for a nominal diameter in millimetres
/// (ISO 261 preferred pitches; identical across the bolt/nut standards
/// above). `None` for sizes not in the table.
const PITCH_MM: &[(f64, f64)] = &[
    (3.0, 0.5),
    (4.0, 0.7),
    (5.0, 0.8),
    (6.0, 1.0),
    (8.0, 1.25),
    (10.0, 1.5),
    (12.0, 1.75),
];

fn pitch_mm(nominal_diameter_mm: f64) -> Option<f64> {
    PITCH_MM
        .iter()
        .find(|(d, _)| *d == nominal_diameter_mm)
        .map(|(_, p)| *p)
}

/// Threaded length `b` of a partially threaded ISO 4014 bolt as a function
/// of the bolt's total (shank) length `l`, all in millimetres.
/// ISO 4014:2011 table 1: `l <= 125`, `125 < l <= 200`, `l > 200`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThreadLength {
    /// Threaded length `b` for total length `l <= 125 mm`.
    pub up_to_125_mm: f64,
    /// Threaded length `b` for `125 mm < l <= 200 mm`.
    pub up_to_200_mm: f64,
    /// Threaded length `b` for `l > 200 mm`.
    pub over_200_mm: f64,
}

impl ThreadLength {
    /// Threaded length for a bolt of total length `length_mm`.
    pub fn for_total_length(&self, length_mm: f64) -> f64 {
        if length_mm <= 125.0 {
            self.up_to_125_mm
        } else if length_mm <= 200.0 {
            self.up_to_200_mm
        } else {
            self.over_200_mm
        }
    }
}

/// Which fastener standard a [`BoltDims`] row cites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoltStyle {
    /// ISO 4762:2012 hexagon socket head cap screw. Fully threaded.
    SocketHeadCapScrew,
    /// ISO 4014:2011 hexagon head bolt with shank (partially threaded).
    HexHeadBolt,
}

impl BoltStyle {
    pub fn citation(self) -> &'static str {
        match self {
            BoltStyle::SocketHeadCapScrew => "ISO 4762:2012",
            BoltStyle::HexHeadBolt => "ISO 4014:2011",
        }
    }
}

/// Head/thread dimensions of a metric bolt, in millimetres.
///
/// Fields left `None` do not apply to the style: socket head cap screws
/// have no across-flats/corners head dims (the drive is an internal hex),
/// while hex head bolts have no fixed head diameter beyond the across
/// corners `e`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoltDims {
    /// Nominal size label, e.g. `"M8"`.
    pub size: &'static str,
    /// Thread nominal diameter `d`, millimetres.
    pub nominal_diameter_mm: f64,
    /// Coarse thread pitch `P`, millimetres.
    pub pitch_mm: f64,
    /// Head height `k`, millimetres (ISO 4762 `k` max; ISO 4014 `k` nominal).
    pub head_height_mm: f64,
    /// Head width across flats `s`, millimetres (hex head only).
    pub head_across_flats_mm: Option<f64>,
    /// Head diameter `dk` max, millimetres (socket head only).
    pub head_diameter_mm: Option<f64>,
    /// Head width across corners `e` min, millimetres (hex head only).
    pub head_across_corners_mm: Option<f64>,
    /// Threaded length tiers `b` for partially threaded bolts; `None` means
    /// fully threaded over the whole length.
    pub thread_length: Option<ThreadLength>,
    /// The ISO standard this row cites.
    pub citation: &'static str,
}

impl BoltDims {
    /// Length of the unthreaded (plain) shank for a bolt of total length
    /// `length_mm`, i.e. `length - threaded_length`. Returns `None` for
    /// fully threaded bolts (there is no shank) or unknown lengths.
    pub fn shank_length_mm(&self, length_mm: f64) -> Option<f64> {
        let threaded = self.thread_length.as_ref()?.for_total_length(length_mm);
        (length_mm - threaded).max(0.0).into()
    }
}

// ISO 4762:2012, Table 1 — hexagon socket head cap screws, M3–M12.
// Columns: size, d, P, k max, dk max.
const SOCKET_HEAD_CAP_SCREWS: &[(f64, f64, f64, f64)] = &[
    (3.0, 0.5, 3.0, 5.5),
    (4.0, 0.7, 4.0, 7.0),
    (5.0, 0.8, 5.0, 8.5),
    (6.0, 1.0, 6.0, 10.0),
    (8.0, 1.25, 8.0, 13.0),
    (10.0, 1.5, 10.0, 16.0),
    (12.0, 1.75, 12.0, 18.0),
];

// ISO 4014:2011, Table 1 — hexagon head bolts with shank, M3–M12.
// Columns: size, d, P, s, e min, k nominal, b(l<=125), b(125<l<=200),
// b(l>200).
const HEX_HEAD_BOLTS: &[(f64, f64, f64, f64, f64, f64, f64, f64)] = &[
    (3.0, 0.5, 5.5, 6.01, 2.0, 12.0, 18.0, 31.0),
    (4.0, 0.7, 7.0, 7.66, 2.8, 14.0, 20.0, 33.0),
    (5.0, 0.8, 8.0, 8.79, 3.5, 16.0, 22.0, 35.0),
    (6.0, 1.0, 10.0, 11.05, 4.0, 18.0, 24.0, 37.0),
    (8.0, 1.25, 13.0, 14.38, 5.3, 22.0, 28.0, 41.0),
    (10.0, 1.5, 16.0, 17.77, 6.4, 26.0, 32.0, 45.0),
    (12.0, 1.75, 18.0, 20.03, 7.5, 30.0, 36.0, 49.0),
];

// ISO 4032:2023, Table 1 — hexagon regular nuts (style 1), M3–M12.
// Columns: size, d, P, s nominal(max), m max.
const HEX_NUTS: &[(f64, f64, f64, f64)] = &[
    (3.0, 0.5, 5.5, 2.4),
    (4.0, 0.7, 7.0, 3.2),
    (5.0, 0.8, 8.0, 4.7),
    (6.0, 1.0, 10.0, 5.2),
    (8.0, 1.25, 13.0, 6.8),
    (10.0, 1.5, 16.0, 8.4),
    (12.0, 1.75, 18.0, 10.8),
];

/// Dimensions of an ISO 4032 hexagon regular nut (style 1), millimetres.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NutDims {
    /// Nominal size label, e.g. `"M8"`.
    pub size: &'static str,
    /// Thread nominal diameter `d`, millimetres.
    pub nominal_diameter_mm: f64,
    /// Coarse thread pitch `P`, millimetres.
    pub pitch_mm: f64,
    /// Width across flats `s` nominal (= max), millimetres.
    pub across_flats_mm: f64,
    /// Nut height `m` max, millimetres.
    pub height_mm: f64,
    /// The ISO standard this row cites.
    pub citation: &'static str,
}

/// ISO 273 hole-fit series.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitClass {
    /// Fine series, hole tolerance H12 (close fit, accurately placed holes).
    Fine,
    /// Medium series, hole tolerance H13 (normal fit, the usual choice).
    Medium,
    /// Coarse series, hole tolerance H14 (loose fit, rough work).
    Coarse,
}

impl FitClass {
    pub fn as_str(self) -> &'static str {
        match self {
            FitClass::Fine => "fine",
            FitClass::Medium => "medium",
            FitClass::Coarse => "coarse",
        }
    }

    /// ISO hole tolerance class for this series.
    pub fn tolerance(self) -> &'static str {
        match self {
            FitClass::Fine => "H12",
            FitClass::Medium => "H13",
            FitClass::Coarse => "H14",
        }
    }
}

/// An ISO 273 clearance hole for a bolt size and fit series, in mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClearanceHole {
    /// Nominal hole diameter `d_h`, millimetres.
    pub diameter_mm: f64,
    /// Hole tolerance class (H12/H13/H14).
    pub tolerance: &'static str,
    /// The ISO standard this row cites.
    pub citation: &'static str,
}

// ISO 273:1991, Table 1 — clearance holes for bolts and screws, M3–M12.
// Columns: size, d, fine (H12), medium (H13), coarse (H14).
const CLEARANCE_HOLES: &[(f64, f64, f64, f64, f64)] = &[
    (3.0, 0.5, 3.2, 3.4, 3.6),
    (4.0, 0.7, 4.3, 4.5, 4.8),
    (5.0, 0.8, 5.3, 5.5, 5.8),
    (6.0, 1.0, 6.4, 6.6, 7.0),
    (8.0, 1.25, 8.4, 9.0, 10.0),
    (10.0, 1.5, 10.5, 11.0, 12.0),
    (12.0, 1.75, 13.0, 13.5, 14.5),
];

pub(crate) fn reference_data() -> serde_json::Value {
    serde_json::json!({
        "pitch_mm": PITCH_MM,
        "socket_head_cap_screws": SOCKET_HEAD_CAP_SCREWS,
        "hex_head_bolts": HEX_HEAD_BOLTS,
        "hex_nuts": HEX_NUTS,
        "clearance_holes": CLEARANCE_HOLES,
    })
}

/// Parse a fastener size label such as `"M8"` into the nominal thread
/// diameter in millimetres. Accepts an optional `m`/`M` prefix case
/// insensitively, surrounding whitespace, and an optional thread suffix
/// separated by `x`, `×`, `*` or whitespace (e.g. `"M8x1.25"`, `"M8 1.25"`);
/// the suffix is ignored. Returns `None` for malformed or non-positive
/// sizes.
pub fn parse_size_label(label: &str) -> Option<f64> {
    let label = label.trim();
    let digits = label
        .strip_prefix('m')
        .or_else(|| label.strip_prefix('M'))?;
    let end = digits
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let rest = &digits[end..];
    if !rest.is_empty()
        && !rest
            .chars()
            .next()
            .is_some_and(|c| matches!(c, 'x' | '×' | '*' | ' ' | '\t'))
    {
        return None;
    }
    let d: f64 = digits[..end].parse().ok()?;
    if !d.is_finite() || d <= 0.0 {
        return None;
    }
    Some(d)
}

/// Look up bolt dimensions for `style` and a size label like `"M8"`.
/// Returns `None` for unlisted sizes (only M3–M12 coarse pitch are
/// tabulated) or malformed labels.
pub fn bolt(style: BoltStyle, size: &str) -> Option<BoltDims> {
    let d = parse_size_label(size)?;
    let pitch = pitch_mm(d)?;
    match style {
        BoltStyle::SocketHeadCapScrew => SOCKET_HEAD_CAP_SCREWS
            .iter()
            .find(|(nominal, _, _, _)| *nominal == d)
            .map(|&(_, _, k, dk)| BoltDims {
                size: size_label(d),
                nominal_diameter_mm: d,
                pitch_mm: pitch,
                head_height_mm: k,
                head_across_flats_mm: None,
                head_diameter_mm: Some(dk),
                head_across_corners_mm: None,
                thread_length: None,
                citation: BoltStyle::SocketHeadCapScrew.citation(),
            }),
        BoltStyle::HexHeadBolt => HEX_HEAD_BOLTS
            .iter()
            .find(|(nominal, _, _, _, _, _, _, _)| *nominal == d)
            .map(|&(_, _, s, e, k, b1, b2, b3)| BoltDims {
                size: size_label(d),
                nominal_diameter_mm: d,
                pitch_mm: pitch,
                head_height_mm: k,
                head_across_flats_mm: Some(s),
                head_diameter_mm: None,
                head_across_corners_mm: Some(e),
                thread_length: Some(ThreadLength {
                    up_to_125_mm: b1,
                    up_to_200_mm: b2,
                    over_200_mm: b3,
                }),
                citation: BoltStyle::HexHeadBolt.citation(),
            }),
    }
}

/// Look up ISO 4032 hex nut dimensions for a size label like `"M8"`.
/// `None` for unlisted sizes or malformed labels.
pub fn nut(size: &str) -> Option<NutDims> {
    let d = parse_size_label(size)?;
    let pitch = pitch_mm(d)?;
    HEX_NUTS
        .iter()
        .find(|(nominal, _, _, _)| *nominal == d)
        .map(|&(_, _, s, m)| NutDims {
            size: size_label(d),
            nominal_diameter_mm: d,
            pitch_mm: pitch,
            across_flats_mm: s,
            height_mm: m,
            citation: "ISO 4032:2023",
        })
}

/// Look up the ISO 273 clearance hole diameter for a bolt size label and
/// fit series. `None` for unlisted sizes or malformed labels.
pub fn clearance_hole(size: &str, fit: FitClass) -> Option<ClearanceHole> {
    let d = parse_size_label(size)?;
    CLEARANCE_HOLES
        .iter()
        .find(|(nominal, _, _, _, _)| *nominal == d)
        .map(|&(_, _, fine, medium, coarse)| {
            let diameter_mm = match fit {
                FitClass::Fine => fine,
                FitClass::Medium => medium,
                FitClass::Coarse => coarse,
            };
            ClearanceHole {
                diameter_mm,
                tolerance: fit.tolerance(),
                citation: "ISO 273:1991",
            }
        })
}

/// Canonical size label for a nominal diameter, e.g. `8.0` -> `"M8"`.
fn size_label(diameter_mm: f64) -> &'static str {
    match diameter_mm {
        3.0 => "M3",
        4.0 => "M4",
        5.0 => "M5",
        6.0 => "M6",
        8.0 => "M8",
        10.0 => "M10",
        12.0 => "M12",
        _ => "M?",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_size_labels() {
        assert_eq!(parse_size_label("M8"), Some(8.0));
        assert_eq!(parse_size_label("m6"), Some(6.0));
        assert_eq!(parse_size_label(" M10 "), Some(10.0));
        assert_eq!(parse_size_label("M8x1.25"), Some(8.0));
        assert_eq!(parse_size_label("M12×1.75"), Some(12.0));
        assert_eq!(parse_size_label("M5*0.8"), Some(5.0));
        assert_eq!(parse_size_label("M3.5"), Some(3.5));
        assert_eq!(parse_size_label("8"), None);
        assert_eq!(parse_size_label("M"), None);
        assert_eq!(parse_size_label("Mx"), None);
        assert_eq!(parse_size_label(""), None);
        assert_eq!(parse_size_label("M8foo"), None);
        assert_eq!(parse_size_label("M0"), None);
        assert_eq!(parse_size_label("M-4"), None);
    }

    #[test]
    fn socket_head_cap_screw_dims() {
        let m8 = bolt(BoltStyle::SocketHeadCapScrew, "M8").unwrap();
        assert_eq!(m8.nominal_diameter_mm, 8.0);
        assert_eq!(m8.pitch_mm, 1.25);
        assert_eq!(m8.head_height_mm, 8.0);
        assert_eq!(m8.head_diameter_mm, Some(13.0));
        assert_eq!(m8.head_across_flats_mm, None);
        assert_eq!(m8.thread_length, None);
        assert_eq!(m8.citation, "ISO 4762:2012");
        // Fully threaded: no shank at any length.
        assert_eq!(m8.shank_length_mm(20.0), None);

        let m3 = bolt(BoltStyle::SocketHeadCapScrew, "M3").unwrap();
        assert_eq!(m3.head_height_mm, 3.0);
        assert_eq!(m3.head_diameter_mm, Some(5.5));
    }

    #[test]
    fn hex_head_bolt_dims_and_thread_length() {
        let m8 = bolt(BoltStyle::HexHeadBolt, "M8").unwrap();
        assert_eq!(m8.nominal_diameter_mm, 8.0);
        assert_eq!(m8.pitch_mm, 1.25);
        assert_eq!(m8.head_across_flats_mm, Some(13.0));
        assert_eq!(m8.head_across_corners_mm, Some(14.38));
        assert_eq!(m8.head_height_mm, 5.3);
        assert_eq!(m8.citation, "ISO 4014:2011");

        let tl = m8.thread_length.unwrap();
        assert_eq!(tl.for_total_length(50.0), 22.0);
        assert_eq!(tl.for_total_length(125.0), 22.0);
        assert_eq!(tl.for_total_length(150.0), 28.0);
        assert_eq!(tl.for_total_length(250.0), 41.0);
        // Shank = total - threaded.
        assert_eq!(m8.shank_length_mm(50.0), Some(28.0));
        assert_eq!(m8.shank_length_mm(150.0), Some(122.0));

        let m12 = bolt(BoltStyle::HexHeadBolt, "M12").unwrap();
        assert_eq!(m12.head_across_flats_mm, Some(18.0));
    }

    #[test]
    fn unknown_sizes_return_none() {
        assert_eq!(bolt(BoltStyle::SocketHeadCapScrew, "M14"), None);
        assert_eq!(bolt(BoltStyle::HexHeadBolt, "M2"), None);
        assert_eq!(bolt(BoltStyle::HexHeadBolt, "garbage"), None);
        assert_eq!(nut("M14"), None);
        assert_eq!(clearance_hole("M14", FitClass::Medium), None);
        assert_eq!(clearance_hole("8", FitClass::Medium), None);
    }

    #[test]
    fn iso273_clearance_holes() {
        assert_eq!(
            clearance_hole("M3", FitClass::Fine).unwrap().diameter_mm,
            3.2
        );
        assert_eq!(
            clearance_hole("M3", FitClass::Medium).unwrap().diameter_mm,
            3.4
        );
        assert_eq!(
            clearance_hole("M3", FitClass::Coarse).unwrap().diameter_mm,
            3.6
        );
        assert_eq!(
            clearance_hole("M8", FitClass::Medium).unwrap().diameter_mm,
            9.0
        );
        assert_eq!(
            clearance_hole("M8", FitClass::Coarse).unwrap().diameter_mm,
            10.0
        );
        assert_eq!(
            clearance_hole("M12", FitClass::Fine).unwrap().diameter_mm,
            13.0
        );
        assert_eq!(
            clearance_hole("M12", FitClass::Medium).unwrap().diameter_mm,
            13.5
        );
        assert_eq!(
            clearance_hole("M12", FitClass::Coarse).unwrap().diameter_mm,
            14.5
        );

        let hole = clearance_hole("M6", FitClass::Medium).unwrap();
        assert_eq!(hole.diameter_mm, 6.6);
        assert_eq!(hole.tolerance, "H13");
        assert_eq!(hole.citation, "ISO 273:1991");
        assert_eq!(FitClass::Fine.tolerance(), "H12");
        assert_eq!(FitClass::Coarse.tolerance(), "H14");
    }

    #[test]
    fn iso4032_nuts() {
        let m8 = nut("M8").unwrap();
        assert_eq!(m8.nominal_diameter_mm, 8.0);
        assert_eq!(m8.pitch_mm, 1.25);
        assert_eq!(m8.across_flats_mm, 13.0);
        assert_eq!(m8.height_mm, 6.8);
        assert_eq!(m8.citation, "ISO 4032:2023");

        let m12 = nut("M12").unwrap();
        assert_eq!(m12.across_flats_mm, 18.0);
        assert_eq!(m12.height_mm, 10.8);
    }
}

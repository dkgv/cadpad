//! ISO 286 limit deviations: the "ISO 286 table" half of rule `fit` and the
//! data `press_fit` needs (todo P2.16, part 1 of 2 — the `fit` rule itself is
//! a later task).
//!
//! Sources (tabulated values, not the ISO 286-1 formula — the formula deviates
//! from the published tables by up to ~13 %):
//! - ISO 286-1:2010, Tables 1–5 — standard tolerance grades (IT) and
//!   fundamental deviations for shafts (this module uses the "grades 3–8"
//!   fundamental-deviation column, which covers every IT grade we tabulate
//!   for the ei-type letters).
//! - ISO 286-2:2010 — republished limit-deviation tables for standard
//!   tolerance classes (H7, h6, g6, …); our computed zones are checked
//!   cell-for-cell against them for the classes covered here.
//!
//! ## Coverage
//!
//! Nominal diameters **0 < d ≤ 50 mm** — the practical subset for press and
//! transition fits on printed/mechanical parts. This is six standard bands:
//! ≤3, >3–6, >6–10, >10–18, >18–30, >30–50 mm. Larger diameters return
//! [`None`]; extend [`IT_GRADES_UM`], [`SHAFT_ES_FGH_UM`], [`SHAFT_EI_KNPS_UM`]
//! and the hole arm of [`hole_deviation_um`] with the ISO 286-1 Tables 1–5
//! values for bands >50–500 mm to widen coverage. Note that beyond 50 mm the
//! ei-type shaft letters (k, n, p, s) switch to finer sub-bands (>50–65,
//! >65–80, …) and need per-sub-band rows, not one value per IT band.
//!
//! Classes covered:
//! - Holes: `H` (the hole-basis letter; EI = 0) at grades IT5–IT12, which
//!   covers H7/H8/H9/H11/H12 and every other `H<n>` grade in the IT table.
//!   Other hole letters (G, JS, K, …) return [`None`] — add ISO 286-1
//!   Tables 2–3 rows when needed.
//! - Shafts: `f`, `g`, `h`, `js`, `k`, `n`, `p`, `s` at IT5–IT12.
//!   `f`/`g`/`h` use grade-independent upper deviations (es); `js` is
//!   symmetric ±IT/2; `k`/`n`/`p`/`s` use lower deviations (ei) that are only
//!   tabulated for IT grades 3–8, so those letters return [`None`] for
//!   grades IT9–IT12.
//!
//! ## `js` representation
//!
//! [`FitClass::letter`] is a single `char`, matching
//! [`super::parse::Parsed::Fit`]. The two-letter shaft class `js` is stored
//! as letter `'j'` — bare `j`/`J` are not tabulated shaft/hole deviations in
//! this module anyway, so `'j'` unambiguously means `js` here. `js` is
//! symmetric: `lower = −IT/2`, `upper = +IT/2` (ISO 286-1 Table 5; e.g.
//! js6 at 10–18 mm = ±5.5 µm). `parse_fit_class("js6")` and
//! `parse_fit_class("JS6")` both yield `letter: 'j'`.
//!
//! ## Units and sign convention
//!
//! Deviations are returned in **micrometres** as `(lower, upper)` offsets
//! from the nominal size: positive = above nominal, negative = below.
//! 1 µm = 0.001 mm (`crate::requirements::quantity::Unit::Um` has factor
//! 0.001 mm, so `value_um * 0.001` gives mm). Shaft and hole zones are both
//! `[nominal + lower, nominal + upper]`.
//!
//! ## Wiring (for the later `fit` / `press_fit` tasks)
//!
//! `parse.rs` already emits [`super::parse::Parsed::Fit`] `{letter, grade,
//! hole}` for tokens like `H7`/`h6`. The intended consumers:
//!
//! - **`fit(feature, class)`** (spec §4.1): parse the class token, resolve the
//!   feature's nominal diameter, then call [`hole_deviation_um`] or
//!   [`shaft_deviation_um`] and check `nominal + lower ≤ measured ≤
//!   nominal + upper`.
//! - **`press_fit(pin, hole, range)`** (P2.15): call
//!   [`clearance_limits_um`] on the hole/shaft classes — a negative result
//!   is interference; `-min_clearance` is the guaranteed interference
//!   magnitude in µm, which the rule checks against `range`.
//! - **Notation lookup** (spec §3.6 `library` source): [`parse_fit_class`]
//!   plus [`fit_kind`] give the Clearance/Transition/Interference label to
//!   display next to a parsed `H7/g6` pair.

/// ISO 286 tolerance class: one letter (fundamental deviation) and one IT
/// grade number. `hole` selects the hole tables (uppercase letter) versus
/// the shaft tables (lowercase letter); [`FitClass::letter`] keeps the case
/// as written for display. See the module docs for the `js` → `'j'`
/// representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FitClass {
    pub letter: char,
    pub grade: u8,
    pub hole: bool,
}

/// Lower/upper limit deviations from nominal size, in micrometres.
/// `lower <= upper`; positive values are above nominal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Deviation {
    pub lower_um: f64,
    pub upper_um: f64,
}

impl Deviation {
    /// Total tolerance-zone width (the IT grade value), in µm.
    pub fn width_um(self) -> f64 {
        self.upper_um - self.lower_um
    }
}

/// Fit character of a hole/shaft pair, per ISO 286-1 §4.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FitKind {
    /// Minimum clearance > 0: always clearance.
    Clearance,
    /// Zones overlap or just touch: clearance or interference possible.
    Transition,
    /// Maximum clearance < 0: always interference.
    Interference,
}

/// Nominal diameter bands in mm: `(exclusive lower, inclusive upper)`.
/// ISO 286 defines these as "over D1 up to and including D2".
const BANDS_MM: [(f64, f64); 6] = [
    (0.0, 3.0),
    (3.0, 6.0),
    (6.0, 10.0),
    (10.0, 18.0),
    (18.0, 30.0),
    (30.0, 50.0),
];

/// Standard tolerance grade values in µm — ISO 286-1:2010 Table 1.
/// `IT_GRADES_UM[band][grade − 5]` for grade in 5..=12, band per
/// [`BANDS_MM`].
const IT_GRADES_UM: [[f64; 8]; 6] = [
    // band:        IT5   IT6   IT7   IT8   IT9   IT10  IT11  IT12   (≤3 mm)
    [4.0, 6.0, 10.0, 14.0, 25.0, 40.0, 60.0, 100.0],
    // >3–6 mm
    [5.0, 8.0, 12.0, 18.0, 30.0, 48.0, 75.0, 120.0],
    // >6–10 mm
    [6.0, 9.0, 15.0, 22.0, 36.0, 58.0, 90.0, 150.0],
    // >10–18 mm
    [8.0, 11.0, 18.0, 27.0, 43.0, 70.0, 110.0, 180.0],
    // >18–30 mm
    [9.0, 13.0, 21.0, 33.0, 52.0, 84.0, 130.0, 210.0],
    // >30–50 mm
    [11.0, 16.0, 25.0, 39.0, 62.0, 100.0, 160.0, 250.0],
];

/// Shaft fundamental deviations es (upper) for letters `f`, `g`, `h` —
/// ISO 286-1:2010 Table 4, the grade-independent rows for a–h, µm.
/// Indexed by [`BANDS_MM`]; `es` is 0 for `h` at every size.
const SHAFT_ES_FGH_UM: [[f64; 3]; 6] = [
    // f      g      h      — band (≤3 / >3–6 / >6–10 / >10–18 / >18–30 / >30–50)
    [-6.0, -2.0, 0.0],
    [-10.0, -4.0, 0.0],
    [-13.0, -5.0, 0.0],
    [-16.0, -6.0, 0.0],
    [-20.0, -7.0, 0.0],
    [-25.0, -9.0, 0.0],
];

/// Shaft fundamental deviations ei (lower) for letters `k`, `n`, `p`, `s` —
/// ISO 286-1:2010 Table 4, the "grades 3–8" column, µm. Indexed by
/// [`BANDS_MM`]. Valid only for IT grades 3–8; for IT9+ these letters have
/// different ei values (and finer >50 mm sub-bands) — [`shaft_deviation_um`]
/// returns [`None`] there.
const SHAFT_EI_KNPS_UM: [[f64; 4]; 6] = [
    // k      n      p      s
    [0.0, 4.0, 6.0, 14.0],   // ≤3
    [1.0, 8.0, 12.0, 19.0],  // >3–6
    [1.0, 10.0, 15.0, 23.0], // >6–10
    [1.0, 12.0, 18.0, 28.0], // >10–18
    [2.0, 15.0, 22.0, 35.0], // >18–30
    [2.0, 17.0, 26.0, 43.0], // >30–50
];

pub(crate) fn reference_data() -> serde_json::Value {
    serde_json::json!({
        "bands_mm": BANDS_MM,
        "it_grades_um": IT_GRADES_UM,
        "shaft_es_fgh_um": SHAFT_ES_FGH_UM,
        "shaft_ei_knps_um": SHAFT_EI_KNPS_UM,
        "source": "ISO 286-1:2010/286-2:2010",
    })
}

/// Band index for `nominal_mm`, or `None` outside 0 < d ≤ 50 mm.
fn band_index(nominal_mm: f64) -> Option<usize> {
    if !nominal_mm.is_finite() || nominal_mm <= 0.0 {
        return None;
    }
    BANDS_MM
        .iter()
        .position(|&(lo, hi)| nominal_mm > lo && nominal_mm <= hi)
}

/// IT grade tolerance width in µm for `grade` (5–12) and `nominal_mm`.
///
/// ISO 286-1:2010 Table 1. Returns `None` for grades outside IT5–IT12 or
/// diameters outside the covered bands (>50 mm).
pub fn it_grade_um(grade: u8, nominal_mm: f64) -> Option<f64> {
    let band = band_index(nominal_mm)?;
    IT_GRADES_UM[band]
        .get((grade.checked_sub(5)?) as usize)
        .copied()
}

/// Limit deviations (µm) for a **hole** class like `H7`, `H11`.
///
/// Only letter `H` is tabulated (EI = 0, zone `[0, +IT]`), which covers the
/// hole-basis system used by the `fit`/`press_fit` rules. Returns `None`
/// for unsupported hole letters, grades outside IT5–IT12, and diameters
/// > 50 mm.
pub fn hole_deviation_um(letter: char, grade: u8, nominal_mm: f64) -> Option<Deviation> {
    if letter.to_ascii_uppercase() != 'H' {
        return None;
    }
    let band = band_index(nominal_mm)?;
    let it = *IT_GRADES_UM[band].get((grade.checked_sub(5)?) as usize)?;
    Some(Deviation {
        lower_um: 0.0,
        upper_um: it,
    })
}

/// Limit deviations (µm) for a **shaft** class like `h6`, `g6`, `js6`, `s6`.
///
/// Supported letters (lowercase; uppercase input is folded):
/// - `f`, `g`, `h`: es-type, zone `[es − IT, es]`, grade-independent es.
/// - `j`: the `js` class (see module docs), symmetric `±IT/2`.
/// - `k`, `n`, `p`, `s`: ei-type, zone `[ei, ei + IT]`, IT grades 3–8 only
///   (returns `None` for IT9–IT12).
///
/// Returns `None` for other letters, unsupported grade/letter combinations,
/// and diameters > 50 mm.
pub fn shaft_deviation_um(letter: char, grade: u8, nominal_mm: f64) -> Option<Deviation> {
    let band = band_index(nominal_mm)?;
    let it = *IT_GRADES_UM[band].get((grade.checked_sub(5)?) as usize)?;
    match letter.to_ascii_lowercase() {
        'f' | 'g' | 'h' => {
            let idx = match letter.to_ascii_lowercase() {
                'f' => 0,
                'g' => 1,
                _ => 2,
            };
            let es = SHAFT_ES_FGH_UM[band][idx];
            Some(Deviation {
                lower_um: es - it,
                upper_um: es,
            })
        }
        'j' => Some(Deviation {
            lower_um: -it / 2.0,
            upper_um: it / 2.0,
        }),
        'k' | 'n' | 'p' | 's' => {
            // ei values are tabulated for IT grades 3–8 only.
            if !(3..=8).contains(&grade) {
                return None;
            }
            let idx = match letter.to_ascii_lowercase() {
                'k' => 0,
                'n' => 1,
                'p' => 2,
                _ => 3,
            };
            let ei = SHAFT_EI_KNPS_UM[band][idx];
            Some(Deviation {
                lower_um: ei,
                upper_um: ei + it,
            })
        }
        _ => None,
    }
}

/// Parse a fit-class token like `H7`, `h6`, `H11`, `js6` into a
/// [`FitClass`].
///
/// The letter's case selects hole vs shaft: uppercase → hole (`H7`),
/// lowercase → shaft (`h6`). `js`/`JS` (two letters) is accepted and stored
/// as `letter: 'j'` — see module docs. Validation is syntactic only;
/// whether deviations are tabulated is decided by
/// [`hole_deviation_um`]/[`shaft_deviation_um`]. Rejects strings containing
/// `/` (use [`parse_fit_pair`] for `H7/g6`), missing grades, and grades
/// outside 1–99.
pub fn parse_fit_class(text: &str) -> Option<FitClass> {
    let t = text.trim();
    // Letters may be one char, or the two-char "js"/"JS" class.
    let letter_len = {
        let mut it = t.chars();
        match (it.next(), it.next(), it.next()) {
            (Some('j' | 'J'), Some('s' | 'S'), Some(c)) if c.is_ascii_digit() => 2,
            (Some(c), _, _) if c.is_ascii_alphabetic() => c.len_utf8(),
            _ => return None,
        }
    };
    let (letters, rest) = t.split_at(letter_len);
    let mut ch = letters.chars();
    let first = ch.next()?;
    if !first.is_ascii_alphabetic() || ch.next().is_some_and(|c| !c.is_ascii_alphabetic()) {
        return None;
    }
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let grade: u8 = rest.parse().ok().filter(|&g| (1..=99).contains(&g))?;
    Some(FitClass {
        letter: first,
        grade,
        hole: first.is_ascii_uppercase(),
    })
}

/// Parse a fit-pair string `H7/g6` (ISO 286 uses `/`; whitespace around the
/// parts is tolerated, `H7-g6` is *not*). Returns `(hole, shaft)` — each
/// side keeps its own case convention.
pub fn parse_fit_pair(text: &str) -> Option<(FitClass, FitClass)> {
    let (a, b) = text.trim().split_once('/')?;
    Some((parse_fit_class(a)?, parse_fit_class(b)?))
}

/// Limit deviations for a parsed [`FitClass`], dispatching on `hole`.
pub fn deviation_um(fit: FitClass, nominal_mm: f64) -> Option<Deviation> {
    if fit.hole {
        hole_deviation_um(fit.letter, fit.grade, nominal_mm)
    } else {
        shaft_deviation_um(fit.letter, fit.grade, nominal_mm)
    }
}

/// Classify a hole/shaft fit at `nominal_mm`: Clearance, Transition or
/// Interference per ISO 286-1 §4.4, computed from the tabulated zones.
///
/// `min_clearance = hole.lower − shaft.upper`,
/// `max_clearance = hole.upper − shaft.lower` (µm; positive = clearance).
/// `min_clearance > 0` → [`FitKind::Clearance`]; `max_clearance < 0` →
/// [`FitKind::Interference`]; otherwise [`FitKind::Transition`] (zones
/// overlap or exactly touch). Returns `None` if either class is untabulated
/// at this diameter.
pub fn fit_kind(hole: FitClass, shaft: FitClass, nominal_mm: f64) -> Option<FitKind> {
    let h = deviation_um(hole, nominal_mm)?;
    let s = deviation_um(shaft, nominal_mm)?;
    let min_clearance = h.lower_um - s.upper_um;
    let max_clearance = h.upper_um - s.lower_um;
    Some(if min_clearance > 0.0 {
        FitKind::Clearance
    } else if max_clearance < 0.0 {
        FitKind::Interference
    } else {
        FitKind::Transition
    })
}

/// Clearance limits of a hole/shaft pair at `nominal_mm`, in µm:
/// `(min_clearance, max_clearance)` where positive values are clearance and
/// negative values are interference. This is what `press_fit` needs: for an
/// interference fit, `-min_clearance` is the smallest guaranteed
/// interference and `-max_clearance` the largest.
///
/// See [`fit_kind`] for the exact formulas. Returns `None` for untabulated
/// classes or diameters > 50 mm.
pub fn clearance_limits_um(hole: FitClass, shaft: FitClass, nominal_mm: f64) -> Option<(f64, f64)> {
    let h = deviation_um(hole, nominal_mm)?;
    let s = deviation_um(shaft, nominal_mm)?;
    Some((h.lower_um - s.upper_um, h.upper_um - s.lower_um))
}

/// Convenience: deviations for both sides of a `"H7/g6"`-style string at
/// `nominal_mm`. Returns `(hole_deviation, shaft_deviation)` in µm, or
/// `None` if the string or either class is unsupported.
pub fn pair_deviations_um(pair: &str, nominal_mm: f64) -> Option<(Deviation, Deviation)> {
    let (h, s) = parse_fit_pair(pair)?;
    Some((deviation_um(h, nominal_mm)?, deviation_um(s, nominal_mm)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(text: &str) -> FitClass {
        parse_fit_class(text).unwrap()
    }

    #[test]
    fn it_grades_match_iso286_table1() {
        // IT7 across all six bands (µm).
        let at = [3.0, 4.0, 8.0, 15.0, 25.0, 45.0];
        let expected = [10.0, 12.0, 15.0, 18.0, 21.0, 25.0];
        for (d, e) in at.iter().zip(expected) {
            assert_eq!(it_grade_um(7, *d), Some(e));
        }
        assert_eq!(it_grade_um(5, 25.0), Some(9.0));
        assert_eq!(it_grade_um(9, 25.0), Some(52.0));
        assert_eq!(it_grade_um(12, 45.0), Some(250.0));
        // Out-of-range grades and diameters.
        assert_eq!(it_grade_um(4, 25.0), None);
        assert_eq!(it_grade_um(13, 25.0), None);
        assert_eq!(it_grade_um(7, 50.5), None);
        assert_eq!(it_grade_um(7, 0.0), None);
        assert_eq!(it_grade_um(7, -1.0), None);
        assert_eq!(it_grade_um(7, f64::NAN), None);
    }

    #[test]
    fn band_boundaries_are_upper_inclusive() {
        // Exactly on a boundary belongs to the lower band ("over 3 to 6").
        assert_eq!(hole_deviation_um('H', 7, 3.0).unwrap().upper_um, 10.0);
        assert_eq!(hole_deviation_um('H', 7, 6.0).unwrap().upper_um, 12.0);
        assert_eq!(hole_deviation_um('H', 7, 6.001).unwrap().upper_um, 15.0);
        assert_eq!(hole_deviation_um('H', 7, 10.0).unwrap().upper_um, 15.0);
        assert_eq!(hole_deviation_um('H', 7, 10.001).unwrap().upper_um, 18.0);
        assert_eq!(hole_deviation_um('H', 7, 50.0).unwrap().upper_um, 25.0);
        assert_eq!(hole_deviation_um('H', 7, 50.1), None);
    }

    #[test]
    fn hole_h_zones() {
        // H letter: lower deviation 0, upper = +IT grade.
        assert_eq!(
            hole_deviation_um('H', 7, 8.0),
            Some(Deviation {
                lower_um: 0.0,
                upper_um: 15.0
            })
        );
        assert_eq!(hole_deviation_um('H', 8, 8.0).unwrap().upper_um, 22.0);
        assert_eq!(hole_deviation_um('H', 9, 8.0).unwrap().upper_um, 36.0);
        assert_eq!(hole_deviation_um('H', 11, 8.0).unwrap().upper_um, 90.0);
        assert_eq!(hole_deviation_um('H', 12, 8.0).unwrap().upper_um, 150.0);
        assert_eq!(hole_deviation_um('H', 7, 25.0).unwrap().upper_um, 21.0);
        // Unsupported hole letters / grades / diameters.
        assert_eq!(hole_deviation_um('G', 7, 25.0), None);
        assert_eq!(hole_deviation_um('H', 6, 25.0).is_some(), true);
        assert_eq!(hole_deviation_um('H', 4, 25.0), None);
    }

    #[test]
    fn shaft_es_classes_fgh() {
        // h: es = 0, zone drops by IT.
        assert_eq!(
            shaft_deviation_um('h', 6, 10.0),
            Some(Deviation {
                lower_um: -9.0,
                upper_um: 0.0
            })
        );
        assert_eq!(shaft_deviation_um('h', 7, 10.0).unwrap().lower_um, -15.0);
        assert_eq!(shaft_deviation_um('h', 6, 25.0).unwrap().lower_um, -13.0);
        // g: es negative.
        assert_eq!(
            shaft_deviation_um('g', 6, 25.0),
            Some(Deviation {
                lower_um: -20.0,
                upper_um: -7.0
            })
        );
        // 8 mm is in band >6–10: es = −5, IT6 = 9 → (−14, −5).
        assert_eq!(
            shaft_deviation_um('g', 6, 8.0),
            Some(Deviation {
                lower_um: -14.0,
                upper_um: -5.0
            })
        );
        // 5 mm is in band >3–6: es = −4.
        assert_eq!(shaft_deviation_um('g', 6, 5.0).unwrap().upper_um, -4.0);
        // f: one step looser than g.
        assert_eq!(
            shaft_deviation_um('f', 6, 25.0),
            Some(Deviation {
                lower_um: -33.0,
                upper_um: -20.0
            })
        );
        // Uppercase input folds to lowercase for shafts.
        assert_eq!(
            shaft_deviation_um('G', 6, 25.0),
            shaft_deviation_um('g', 6, 25.0)
        );
    }

    #[test]
    fn shaft_js_is_symmetric() {
        // js6 at 10–18 mm: IT6 = 11 → ±5.5 µm (ISO 286-1 Table 5).
        assert_eq!(
            shaft_deviation_um('j', 6, 15.0),
            Some(Deviation {
                lower_um: -5.5,
                upper_um: 5.5
            })
        );
        // js6 at >3–6: IT6 = 8 → ±4.
        assert_eq!(
            shaft_deviation_um('j', 6, 5.0),
            Some(Deviation {
                lower_um: -4.0,
                upper_um: 4.0
            })
        );
    }

    #[test]
    fn shaft_ei_classes_knps() {
        // Values from ISO 286-1 Table 4 (grades 3–8 column), zones [ei, ei+IT].
        assert_eq!(
            shaft_deviation_um('k', 6, 25.0),
            Some(Deviation {
                lower_um: 2.0,
                upper_um: 15.0
            })
        );
        assert_eq!(
            shaft_deviation_um('n', 6, 25.0),
            Some(Deviation {
                lower_um: 15.0,
                upper_um: 28.0
            })
        );
        assert_eq!(
            shaft_deviation_um('p', 6, 25.0),
            Some(Deviation {
                lower_um: 22.0,
                upper_um: 35.0
            })
        );
        assert_eq!(
            shaft_deviation_um('s', 6, 25.0),
            Some(Deviation {
                lower_um: 35.0,
                upper_um: 48.0
            })
        );
        // Small sizes.
        assert_eq!(
            shaft_deviation_um('s', 6, 4.0),
            Some(Deviation {
                lower_um: 19.0,
                upper_um: 27.0
            })
        );
        assert_eq!(shaft_deviation_um('k', 6, 10.0).unwrap().lower_um, 1.0);
        // ei letters only tabulated for IT grades 3–8.
        assert_eq!(shaft_deviation_um('n', 9, 25.0), None);
        assert_eq!(shaft_deviation_um('s', 11, 25.0), None);
        // Unsupported letters.
        assert_eq!(shaft_deviation_um('z', 6, 25.0), None);
        assert_eq!(shaft_deviation_um('a', 6, 25.0), None);
    }

    #[test]
    fn parse_fit_class_tokens() {
        assert_eq!(
            parse_fit_class("H7"),
            Some(FitClass {
                letter: 'H',
                grade: 7,
                hole: true
            })
        );
        assert_eq!(
            parse_fit_class("h6"),
            Some(FitClass {
                letter: 'h',
                grade: 6,
                hole: false
            })
        );
        assert_eq!(
            parse_fit_class(" H11 "),
            Some(FitClass {
                letter: 'H',
                grade: 11,
                hole: true
            })
        );
        // js folds to 'j'.
        assert_eq!(
            parse_fit_class("js6"),
            Some(FitClass {
                letter: 'j',
                grade: 6,
                hole: false
            })
        );
        assert_eq!(parse_fit_class("7H"), None);
        assert_eq!(parse_fit_class("H"), None);
        assert_eq!(parse_fit_class("H7/g6"), None);
        assert_eq!(parse_fit_class(""), None);
        assert_eq!(parse_fit_pair("H7/g6"), Some((class("H7"), class("g6"))));
        assert_eq!(
            parse_fit_pair(" H7 / js6 "),
            Some((class("H7"), class("js6")))
        );
        assert_eq!(parse_fit_pair("h7/H6"), Some((class("h7"), class("H6"))));
        assert_eq!(parse_fit_pair("H7"), None);
        assert_eq!(parse_fit_pair("H7-g6"), None);
    }

    #[test]
    fn fit_kind_classification() {
        let h7 = class("H7");
        // H7/g6: always clearance.
        assert_eq!(fit_kind(h7, class("g6"), 25.0), Some(FitKind::Clearance));
        // H7/h6: min clearance = 0 − 0 = 0 → transition (zones touch).
        assert_eq!(fit_kind(h7, class("h6"), 25.0), Some(FitKind::Transition));
        // H7/k6, H7/js6, H7/n6: overlapping zones → transition.
        assert_eq!(fit_kind(h7, class("k6"), 25.0), Some(FitKind::Transition));
        assert_eq!(fit_kind(h7, class("js6"), 25.0), Some(FitKind::Transition));
        assert_eq!(fit_kind(h7, class("n6"), 25.0), Some(FitKind::Transition));
        // H7/p6 and H7/s6: always interference.
        assert_eq!(fit_kind(h7, class("p6"), 25.0), Some(FitKind::Interference));
        assert_eq!(fit_kind(h7, class("s6"), 25.0), Some(FitKind::Interference));
        // Untabulated at this diameter.
        assert_eq!(fit_kind(h7, class("h6"), 80.0), None);
    }

    #[test]
    fn clearance_limits_worked_examples() {
        // Worked example at 25 mm (band >18–30):
        // H7 hole = +0/+21, p6 shaft = +22/+35 → interference 1–35 µm,
        // i.e. clearance limits (−35, −1).
        let (min, max) = clearance_limits_um(class("H7"), class("p6"), 25.0).unwrap();
        assert_eq!((min, max), (-35.0, -1.0));
        // H7/g6 at 25: hole +0/+21, shaft −20/−7 → clearance 7–41 µm.
        let (min, max) = clearance_limits_um(class("H7"), class("g6"), 25.0).unwrap();
        assert_eq!((min, max), (7.0, 41.0));
        // H7/n6 at 25: hole +0/+21, shaft +15/+28 → −28…+6 (transition).
        let (min, max) = clearance_limits_um(class("H7"), class("n6"), 25.0).unwrap();
        assert_eq!((min, max), (-28.0, 6.0));
    }

    #[test]
    fn pair_deviations_end_to_end() {
        let (h, s) = pair_deviations_um("H7/g6", 8.0).unwrap();
        assert_eq!(
            h,
            Deviation {
                lower_um: 0.0,
                upper_um: 15.0
            }
        );
        assert_eq!(
            s,
            Deviation {
                lower_um: -14.0,
                upper_um: -5.0
            }
        );
        assert_eq!(pair_deviations_um("H7", 8.0), None);
        assert_eq!(pair_deviations_um("H7/Z9", 8.0), None);
        assert_eq!(pair_deviations_um("H7/g6", 99.0), None);
    }
}

//! Prose quantity parser. Finds quantities, ranges, tolerances, metric thread
//! sizes (`M8`) and ISO fit classes (`H7`) in free text and reports their
//! spans, so provenance can later check that a claim's number appears in the
//! claim's prose span.
//!
//! Spans are character (Unicode scalar) offsets, matching [`super::Span`].
//! The parser is deliberately conservative: it prefers to miss a number over
//! inventing one, because a missed number is reported as unsourced (and asked
//! about) while an invented one would pass as `text`.
use super::quantity::{Dimension, Quantity, Unit};
use serde::Serialize;

/// A number that may be written without a unit (`±0.1`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Amount {
    pub value: f64,
    pub unit: Option<Unit>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Parsed {
    /// `8 mm`, `110°`, `half a millimetre`.
    Quantity(Quantity),
    /// `0.2–0.4 mm`, `0.2 mm to 0.4 mm`; written order, may have lo > hi.
    Range { lo: Quantity, hi: Quantity },
    /// `±0.1 mm`, `+0/+15 µm`. Offsets are signed, `upper >= lower`.
    Tolerance {
        upper: Amount,
        lower: Amount,
        symmetric: bool,
    },
    /// Metric thread size: `M8`, `M2.5`, `M8x1.25`.
    Thread {
        diameter_mm: f64,
        pitch_mm: Option<f64>,
    },
    /// ISO 286 fit class: `H7` (hole, uppercase) or `h6` (shaft, lowercase).
    Fit { letter: char, grade: u8, hole: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Match {
    /// `[start, end)` in characters.
    pub span: [usize; 2],
    pub value: Parsed,
}

/// A numeric token, with or without a unit.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct NumberToken {
    pub span: [usize; 2],
    pub value: f64,
}

type Chars<'a> = &'a [char];

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
fn at(c: Chars, i: usize) -> Option<char> {
    c.get(i).copied()
}
fn is_digit(c: Option<char>) -> bool {
    matches!(c, Some(d) if d.is_ascii_digit())
}
fn is_minus(c: char) -> bool {
    c == '-' || c == '−'
}
fn is_space(c: char) -> bool {
    c == ' ' || c == '\u{a0}' || c == '\u{202f}' || c == '\u{2009}'
}
fn skip_space(c: Chars, mut i: usize) -> usize {
    while at(c, i).is_some_and(is_space) {
        i += 1;
    }
    i
}
fn starts_with(c: Chars, i: usize, s: &str, ci: bool) -> bool {
    let mut j = i;
    for ch in s.chars() {
        match at(c, j) {
            Some(x) if x == ch || (ci && x.to_lowercase().eq(ch.to_lowercase())) => j += 1,
            _ => return false,
        }
    }
    true
}

enum Num {
    None,
    /// Looks numeric but is ambiguous (`1,5`); skip to the end, report nothing.
    Invalid(usize),
    Ok(f64, usize),
}

/// Unsigned number at `i`: `8`, `0.5`, `.5`, `1e-5`, `1,000`. A trailing
/// fraction (`1/2`) is handled by the caller.
fn number_at(c: Chars, i: usize) -> Num {
    let start_digit = is_digit(at(c, i));
    if !start_digit && !(at(c, i) == Some('.') && is_digit(at(c, i + 1))) {
        return Num::None;
    }
    let mut j = i;
    let mut text = String::new();
    while is_digit(at(c, j)) {
        text.push(c[j]);
        j += 1;
    }
    let lead = j - i;
    if at(c, j) == Some(',') && is_digit(at(c, j + 1)) {
        // Thousands groups only: 1,000 / 12,345,678.
        let mut ok = (1..=3).contains(&lead) && start_digit;
        let mut k = j;
        while at(c, k) == Some(',') && is_digit(at(c, k + 1)) {
            let g = k + 1;
            let mut e = g;
            while is_digit(at(c, e)) {
                e += 1;
            }
            if e - g != 3 {
                ok = false;
            }
            if ok {
                text.extend(&c[g..e]);
            }
            k = e;
        }
        if !ok {
            return Num::Invalid(k);
        }
        j = k;
    }
    if at(c, j) == Some('.') && is_digit(at(c, j + 1)) {
        text.push('.');
        j += 1;
        while is_digit(at(c, j)) {
            text.push(c[j]);
            j += 1;
        }
    }
    if matches!(at(c, j), Some('e' | 'E')) {
        let mut k = j + 1;
        let mut exp = String::from("e");
        if matches!(at(c, k), Some('+')) || at(c, k).is_some_and(is_minus) {
            exp.push(if at(c, k) == Some('+') { '+' } else { '-' });
            k += 1;
        }
        if is_digit(at(c, k)) {
            while is_digit(at(c, k)) {
                exp.push(c[k]);
                k += 1;
            }
            text.push_str(&exp);
            j = k;
        }
    }
    if text.starts_with('.') {
        text.insert(0, '0');
    }
    match text.parse::<f64>() {
        Ok(v) if v.is_finite() => Num::Ok(v, j),
        _ => Num::Invalid(j),
    }
}

/// `(alias, unit, case_insensitive)`.
const ALIASES: &[(&str, Unit, bool)] = &[
    ("mm", Unit::Mm, false),
    ("millimetres", Unit::Mm, true),
    ("millimetre", Unit::Mm, true),
    ("millimeters", Unit::Mm, true),
    ("millimeter", Unit::Mm, true),
    ("cm", Unit::Cm, false),
    ("centimetres", Unit::Cm, true),
    ("centimetre", Unit::Cm, true),
    ("centimeters", Unit::Cm, true),
    ("centimeter", Unit::Cm, true),
    ("m", Unit::M, false),
    ("metres", Unit::M, true),
    ("metre", Unit::M, true),
    ("meters", Unit::M, true),
    ("meter", Unit::M, true),
    ("µm", Unit::Um, false),
    ("μm", Unit::Um, false),
    ("um", Unit::Um, false),
    ("microns", Unit::Um, true),
    ("micron", Unit::Um, true),
    ("micrometres", Unit::Um, true),
    ("micrometre", Unit::Um, true),
    ("micrometers", Unit::Um, true),
    ("micrometer", Unit::Um, true),
    ("in", Unit::In, false),
    ("inches", Unit::In, true),
    ("inch", Unit::In, true),
    ("\"", Unit::In, false),
    ("deg", Unit::Deg, false),
    ("degrees", Unit::Deg, true),
    ("degree", Unit::Deg, true),
    ("°", Unit::Deg, false),
    ("rad", Unit::Rad, false),
    ("radians", Unit::Rad, true),
    ("radian", Unit::Rad, true),
    ("mm²", Unit::Mm2, false),
    ("mm2", Unit::Mm2, false),
    ("mm^2", Unit::Mm2, false),
    ("cm²", Unit::Cm2, false),
    ("cm2", Unit::Cm2, false),
    ("cm^2", Unit::Cm2, false),
    ("mm³", Unit::Mm3, false),
    ("mm3", Unit::Mm3, false),
    ("mm^3", Unit::Mm3, false),
    ("cm³", Unit::Cm3, false),
    ("cm3", Unit::Cm3, false),
    ("cm^3", Unit::Cm3, false),
    ("g", Unit::G, false),
    ("grams", Unit::G, true),
    ("gram", Unit::G, true),
    ("kg", Unit::Kg, false),
    ("kilograms", Unit::Kg, true),
    ("kilogram", Unit::Kg, true),
    ("s", Unit::S, false),
    ("sec", Unit::S, false),
    ("seconds", Unit::S, true),
    ("second", Unit::S, true),
    ("N", Unit::N, false),
    ("newtons", Unit::N, true),
    ("newton", Unit::N, true),
    ("N·s", Unit::Ns, false),
    ("N s", Unit::Ns, false),
    ("N*s", Unit::Ns, false),
    ("N-s", Unit::Ns, false),
];

/// Longest unit alias at `i` that ends on a word boundary. `spaced` says
/// whether whitespace separated it from the number.
fn unit_at(c: Chars, i: usize, spaced: bool) -> Option<(Unit, usize)> {
    let mut best: Option<(Unit, usize)> = None;
    for &(alias, unit, ci) in ALIASES {
        if !starts_with(c, i, alias, ci) {
            continue;
        }
        let end = i + alias.chars().count();
        if best.is_some_and(|(_, e)| e >= end) {
            continue;
        }
        let next = at(c, end);
        if alias != "\"" && next.is_some_and(is_word) {
            continue;
        }
        // Rates (`mm/s`, `N/mm`) are not quantities of this unit.
        if next == Some('/') && at(c, end + 1).is_some_and(char::is_alphabetic) {
            continue;
        }
        if alias == "in"
            && spaced
            && !matches!(next, None | Some('.' | ',' | ';' | ':' | '!' | '?' | ')'))
        {
            continue;
        }
        best = Some((unit, end));
    }
    best
}

/// A unit written alone (`"mm"`, `"deg"`, `"µm"`), as parameter metadata does.
pub fn parse_unit(token: &str) -> Option<Unit> {
    let c: Vec<char> = token.trim().chars().collect();
    unit_at(&c, 0, false)
        .filter(|(_, e)| *e == c.len())
        .map(|(u, _)| u)
}

fn unit_after(c: Chars, j: usize) -> Option<(Unit, usize)> {
    let k = skip_space(c, j);
    unit_at(c, k, k > j)
}

const NUMBER_WORDS: &[(&str, f64)] = &[
    ("zero", 0.0),
    ("one", 1.0),
    ("two", 2.0),
    ("three", 3.0),
    ("four", 4.0),
    ("five", 5.0),
    ("six", 6.0),
    ("seven", 7.0),
    ("eight", 8.0),
    ("nine", 9.0),
    ("ten", 10.0),
    ("eleven", 11.0),
    ("twelve", 12.0),
    ("thirteen", 13.0),
    ("fourteen", 14.0),
    ("fifteen", 15.0),
    ("sixteen", 16.0),
    ("seventeen", 17.0),
    ("eighteen", 18.0),
    ("nineteen", 19.0),
    ("twenty", 20.0),
];

/// `half a`, `a quarter of a`, `five`... followed by a unit.
fn word_quantity_at(c: Chars, i: usize) -> Option<(Quantity, usize)> {
    if i > 0 && at(c, i - 1).is_some_and(is_word) {
        return None;
    }
    let word_end = |s: &str| -> Option<usize> {
        if starts_with(c, i, s, true) {
            let e = i + s.chars().count();
            if !at(c, e).is_some_and(is_word) {
                return Some(e);
            }
        }
        None
    };
    let mut candidates: Vec<(f64, usize)> = Vec::new();
    for article in ["a", "an"] {
        if let Some(e) = word_end(&format!("half {article}")) {
            candidates.push((0.5, e));
        }
        if let Some(e) = word_end(&format!("a quarter of {article}")) {
            candidates.push((0.25, e));
        }
    }
    for (word, v) in NUMBER_WORDS {
        if let Some(e) = word_end(word) {
            candidates.push((*v, e));
        }
    }
    let (value, end) = candidates.into_iter().max_by_key(|(_, e)| *e)?;
    let k = skip_space(c, end);
    if k == end {
        return None;
    }
    let (unit, uend) = unit_at(c, k, true)?;
    Some((Quantity::new(value, unit), uend))
}

/// Optional sign in front of a number at `i`, valid only after a non-word char.
fn sign_at(c: Chars, i: usize) -> Option<f64> {
    let ch = at(c, i)?;
    if !(ch == '+' || is_minus(ch))
        || !is_digit(at(c, i + 1)) && !(at(c, i + 1) == Some('.') && is_digit(at(c, i + 2)))
    {
        return None;
    }
    if i > 0 && at(c, i - 1).is_some_and(|p| is_word(p) || p == '.') {
        return None;
    }
    Some(if ch == '+' { 1.0 } else { -1.0 })
}

/// `n` or `a/b` at `i` (unsigned). Fractions only when no decimal part.
fn value_at(c: Chars, i: usize) -> Num {
    match number_at(c, i) {
        Num::Ok(v, e) => {
            let integral = c[i..e].iter().all(|ch| ch.is_ascii_digit());
            if integral
                && at(c, e) == Some('/')
                && is_digit(at(c, e + 1))
                && let Num::Ok(d, e2) = number_at(c, e + 1)
            {
                if d != 0.0 && c[e + 1..e2].iter().all(|ch| ch.is_ascii_digit()) {
                    return Num::Ok(v / d, e2);
                }
                return Num::Invalid(e2);
            }
            Num::Ok(v, e)
        }
        other => other,
    }
}

fn amount(value: f64, unit: Option<Unit>) -> Amount {
    Amount { value, unit }
}

fn tolerance_at(c: Chars, i: usize) -> Option<(Parsed, usize)> {
    if i > 0 && at(c, i - 1).is_some_and(|p| is_word(p) || p == '.') && at(c, i) != Some('±') {
        return None;
    }
    // Symmetric: ±x, +/-x, +-x, plus or minus x.
    let mut sym_end = None;
    if at(c, i) == Some('±') {
        sym_end = Some(i + 1);
    } else if at(c, i) == Some('+') {
        if at(c, i + 1) == Some('/') && at(c, i + 2).is_some_and(is_minus) {
            sym_end = Some(i + 3);
        } else if at(c, i + 1).is_some_and(is_minus) {
            sym_end = Some(i + 2);
        }
    } else if starts_with(c, i, "plus or minus", true) && !at(c, i + 13).is_some_and(is_word) {
        sym_end = Some(i + 13);
    }
    if let Some(e) = sym_end {
        let k = skip_space(c, e);
        if let Num::Ok(v, ne) = number_at(c, k) {
            let unit = unit_after(c, ne);
            let end = unit.map_or(ne, |(_, e)| e);
            return Some((
                Parsed::Tolerance {
                    upper: amount(v, unit.map(|u| u.0)),
                    lower: amount(-v, unit.map(|u| u.0)),
                    symmetric: true,
                },
                end,
            ));
        }
        return None;
    }
    // Asymmetric: +a/-b unit. First sign explicit, unit required.
    let s1 = sign_at(c, i)?;
    let Num::Ok(a, e1) = number_at(c, i + 1) else {
        return None;
    };
    if at(c, e1) != Some('/') {
        return None;
    }
    let mut k = e1 + 1;
    let s2 = match at(c, k) {
        Some('+') => {
            k += 1;
            1.0
        }
        Some(m) if is_minus(m) => {
            k += 1;
            -1.0
        }
        _ => 1.0,
    };
    let Num::Ok(b, e2) = number_at(c, k) else {
        return None;
    };
    let (unit, end) = unit_after(c, e2)?;
    let (x, y) = (s1 * a, s2 * b);
    let (upper, lower) = if x >= y { (x, y) } else { (y, x) };
    Some((
        Parsed::Tolerance {
            upper: amount(upper, Some(unit)),
            lower: amount(lower, Some(unit)),
            symmetric: false,
        },
        end,
    ))
}

fn thread_at(c: Chars, i: usize) -> Option<(Parsed, usize)> {
    if at(c, i) != Some('M')
        || (i > 0 && at(c, i - 1).is_some_and(is_word))
        || !is_digit(at(c, i + 1))
    {
        return None;
    }
    let Num::Ok(d, mut e) = number_at(c, i + 1) else {
        return None;
    };
    if d < 1.0 || d > 200.0 || c[i + 1] == '0' {
        return None;
    }
    let mut pitch = None;
    if matches!(at(c, e), Some('x' | '×')) {
        let Num::Ok(p, pe) = number_at(c, e + 1) else {
            return None;
        };
        pitch = Some(p);
        e = pe;
    }
    if at(c, e).is_some_and(is_word) || (at(c, e) == Some('.') && is_digit(at(c, e + 1))) {
        return None;
    }
    Some((
        Parsed::Thread {
            diameter_mm: d,
            pitch_mm: pitch,
        },
        e,
    ))
}

/// Lowest IT grade read as a fit class (`H5`); `E1`..`R4` are labels.
const FIT_MIN_GRADE: u32 = 5;

/// Words after which a letter+digit token names an item, not a fit class.
const IDENTIFIER_CUES: &[&str] = &[
    "case",
    "req",
    "requirement",
    "step",
    "section",
    "fig",
    "figure",
    "rev",
    "revision",
    "test",
    "item",
    "table",
    "ref",
    "no",
    "sheet",
    "ticket",
    "issue",
    "task",
];

/// Rule for `E5`/`R6`-style tokens that are clearly identifiers: the token is
/// preceded by an identifier cue word ("case E6", "step P5"), is followed by
/// `:` (a label, "E5: lid fits"), or is wrapped in brackets ("(E5)").
fn looks_like_identifier(c: Chars, start: usize, end: usize) -> bool {
    if at(c, end) == Some(':') {
        return true;
    }
    if start > 0 && matches!(c[start - 1], '(' | '[') && matches!(at(c, end), Some(')' | ']')) {
        return true;
    }
    let mut k = start;
    while k > 0 && is_space(c[k - 1]) {
        k -= 1;
    }
    let word_end = k;
    while k > 0 && c[k - 1].is_alphabetic() {
        k -= 1;
    }
    if k == word_end {
        return false;
    }
    let word: String = c[k..word_end].iter().collect::<String>().to_lowercase();
    IDENTIFIER_CUES.contains(&word.as_str())
}

fn fit_at(c: Chars, i: usize) -> Option<(Parsed, usize)> {
    let letter = at(c, i)?;
    const UPPER: &str = "ABCDEFGHJKNPRSTUXYZ";
    const LOWER: &str = "abcdefghjkmnprstuxyz";
    let hole = letter.is_ascii_uppercase();
    if !(if hole { UPPER } else { LOWER }).contains(letter) {
        return None;
    }
    if i > 0 && at(c, i - 1).is_some_and(is_word) {
        return None;
    }
    let mut e = i + 1;
    let mut grade = 0u32;
    while is_digit(at(c, e)) && e - i <= 3 {
        grade = grade * 10 + c[e].to_digit(10)?;
        e += 1;
    }
    let digits = e - i - 1;
    // Grades 1-4 are gauge grades, never used for design fits; they are far
    // more likely to be labels such as E1 or R2.
    if digits == 0 || digits > 2 || c[i + 1] == '0' || !(FIT_MIN_GRADE..=18).contains(&grade) {
        return None;
    }
    if at(c, e).is_some_and(is_word) || (at(c, e) == Some('.') && is_digit(at(c, e + 1))) {
        return None;
    }
    if looks_like_identifier(c, i, e) {
        return None;
    }
    Some((
        Parsed::Fit {
            letter,
            grade: grade as u8,
            hole,
        },
        e,
    ))
}

fn is_range_sep(c: Chars, i: usize) -> Option<(usize, bool)> {
    // Returns (end of separator, signed second operand allowed).
    match at(c, i)? {
        '-' | '–' | '—' => Some((i + 1, false)),
        '…' => Some((i + 1, true)),
        '.' if at(c, i + 1) == Some('.') => Some((i + 2, true)),
        't' | 'T' if starts_with(c, i, "to", true) && !at(c, i + 2).is_some_and(is_word) => {
            Some((i + 2, true))
        }
        _ => None,
    }
}

/// Numeric quantity or range starting at `i` (sign included).
/// `Err(next)` means nothing to report; continue scanning at `next`.
fn numeric_at(c: Chars, i: usize) -> Result<(Parsed, usize), usize> {
    let mut start = i;
    let mut sign = 1.0;
    if let Some(s) = sign_at(c, i) {
        sign = s;
        start += 1;
    } else if !is_digit(at(c, i)) && !(at(c, i) == Some('.') && is_digit(at(c, i + 1))) {
        return Err(i + 1);
    }
    if i == start && i > 0 {
        let p = c[i - 1];
        if p.is_alphanumeric() || p == '_' {
            return Err(skip_token(c, i));
        }
        if (p == '.' || p == ',') && i > 1 && c[i - 2].is_ascii_digit() {
            return Err(skip_token(c, i));
        }
    }
    let (v1, e1) = match value_at(c, start) {
        Num::Ok(v, e) => (sign * v, e),
        Num::Invalid(e) => return Err(e),
        Num::None => return Err(i + 1),
    };
    let u1 = unit_after(c, e1);
    let after1 = u1.map_or(e1, |u| u.1);
    // Range continuation.
    let k = skip_space(c, after1);
    if let Some((sep_end, signed_ok)) = is_range_sep(c, k) {
        let mut m = skip_space(c, sep_end);
        let mut s2 = 1.0;
        if signed_ok && let Some(s) = sign_at_loose(c, m) {
            s2 = s;
            m += 1;
        }
        if let Num::Ok(v, e2) = value_at(c, m) {
            let u2 = unit_after(c, e2);
            let lo_unit = u1.map(|u| u.0).or(u2.map(|u| u.0));
            if let (Some(lu), Some((hu, end))) = (lo_unit, u2)
                && lu.dimension() == hu.dimension()
            {
                return Ok((
                    Parsed::Range {
                        lo: Quantity::new(v1, lu),
                        hi: Quantity::new(s2 * v, hu),
                    },
                    end,
                ));
            }
        }
    }
    match u1 {
        Some((unit, end)) => Ok((Parsed::Quantity(Quantity::new(v1, unit)), end)),
        None => Err(e1),
    }
}

fn sign_at_loose(c: Chars, i: usize) -> Option<f64> {
    match at(c, i)? {
        '+' if is_digit(at(c, i + 1)) => Some(1.0),
        m if is_minus(m) && is_digit(at(c, i + 1)) => Some(-1.0),
        _ => None,
    }
}

/// End of the alphanumeric/decimal token containing `i`.
fn skip_token(c: Chars, mut i: usize) -> usize {
    while at(c, i).is_some_and(|ch| ch.is_alphanumeric() || ch == '_') {
        i += 1;
    }
    i.max(1)
}

/// All quantities, ranges, tolerances, thread sizes and fit classes in `text`,
/// in order of appearance.
pub fn parse_quantities(text: &str) -> Vec<Match> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let found = tolerance_at(&c, i)
            .or_else(|| thread_at(&c, i))
            .or_else(|| fit_at(&c, i))
            .or_else(|| word_quantity_at(&c, i).map(|(q, e)| (Parsed::Quantity(q), e)));
        if let Some((value, end)) = found {
            out.push(Match {
                span: [i, end],
                value,
            });
            i = end;
            continue;
        }
        match numeric_at(&c, i) {
            Ok((value, end)) => {
                out.push(Match {
                    span: [i, end],
                    value,
                });
                i = end;
            }
            Err(next) => i = next.max(i + 1),
        }
    }
    out
}

/// Every numeric token (`5`, `-2.5`, `1,000`, `1e-5`), with or without a unit.
/// Provenance uses this for unit-less values such as step counts.
pub fn find_numbers(text: &str) -> Vec<NumberToken> {
    let c: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let mut start = i;
        let mut sign = 1.0;
        if let Some(s) = sign_at(&c, i) {
            sign = s;
            start += 1;
        }
        let blocked = i == start
            && i > 0
            && (c[i - 1].is_alphanumeric()
                || c[i - 1] == '_'
                || (matches!(c[i - 1], '.' | ',') && i > 1 && c[i - 2].is_ascii_digit()));
        match (blocked, number_at(&c, start)) {
            (false, Num::Ok(v, e)) => {
                out.push(NumberToken {
                    span: [i, e],
                    value: sign * v,
                });
                i = e;
            }
            (_, Num::Invalid(e)) => i = e.max(i + 1),
            _ => i += 1,
        }
    }
    out
}

impl Parsed {
    /// Values this match offers for a provenance check, normalised, with their
    /// dimension (`None` for unit-less amounts).
    fn candidates(&self) -> Vec<(Option<Dimension>, f64, Option<Quantity>)> {
        let q = |x: Quantity| (Some(x.dimension()), x.internal_value(), Some(x));
        match self {
            Parsed::Quantity(x) => vec![q(*x)],
            Parsed::Range { lo, hi } => vec![q(*lo), q(*hi)],
            Parsed::Tolerance { upper, lower, .. } => [upper, lower]
                .into_iter()
                .flat_map(|a| match a.unit {
                    Some(u) => {
                        let x = Quantity::new(a.value, u);
                        vec![q(x), q(Quantity::new(-a.value, u))]
                    }
                    None => vec![(None, a.value, None), (None, -a.value, None)],
                })
                .collect(),
            Parsed::Thread {
                diameter_mm,
                pitch_mm,
            } => {
                let mut v = vec![q(Quantity::new(*diameter_mm, Unit::Mm))];
                if let Some(p) = pitch_mm {
                    v.push(q(Quantity::new(*p, Unit::Mm)));
                }
                v
            }
            Parsed::Fit { .. } => vec![],
        }
    }
}

/// Does `text[span]` contain the token a claim argument names? Thread sizes
/// (`M8`) match case-insensitively and with a plural `s` ("m8s"); every other
/// token (`H7`, `as_modelled`) must appear exactly, as a whole word.
pub fn token_in_span(text: &str, span: [usize; 2], token: &str) -> bool {
    let slice: Vec<char> = text
        .chars()
        .skip(span[0])
        .take(span[1].saturating_sub(span[0]))
        .collect();
    let tok: Vec<char> = token.chars().collect();
    if tok.is_empty() {
        return false;
    }
    let thread_like = tok[0] == 'M'
        && tok.len() > 1
        && tok[1..]
            .iter()
            .all(|c| c.is_ascii_digit() || *c == '.' || *c == 'x');
    let eq = |a: char, b: char| {
        if thread_like {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    };
    if slice.len() < tok.len() {
        return false;
    }
    (0..=slice.len() - tok.len()).any(|s| {
        if !tok.iter().enumerate().all(|(k, t)| eq(slice[s + k], *t)) {
            return false;
        }
        if s > 0 && is_word(slice[s - 1]) {
            return false;
        }
        let e = s + tok.len();
        match slice.get(e) {
            None => true,
            Some(c) if !is_word(*c) => true,
            Some('s' | 'S') if thread_like => !slice.get(e + 1).is_some_and(|c| is_word(*c)),
            _ => false,
        }
    })
}

/// Does `text[span]` contain a value equal to `quantity` after unit
/// normalisation? This is the `text` provenance check (spec §3.6). Unit-less
/// numbers in the prose match a quantity written in the same raw value
/// (`±0.1`, `step 5`) when the claim's unit is dimensionless or the prose
/// number has no unit of its own.
pub fn quantity_in_span(text: &str, span: [usize; 2], quantity: &Quantity) -> bool {
    let slice: String = text
        .chars()
        .skip(span[0])
        .take(span[1].saturating_sub(span[0]))
        .collect();
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-12);
    for m in parse_quantities(&slice) {
        for (dim, internal, _) in m.value.candidates() {
            match dim {
                Some(d)
                    if d == quantity.dimension() && close(internal, quantity.internal_value()) =>
                {
                    return true;
                }
                None if close(internal.abs(), quantity.value.abs()) => return true,
                _ => {}
            }
        }
    }
    // Bare numbers count only for dimensionless claims ("step 5", "5 to 15").
    if quantity.unit == Unit::One {
        return find_numbers(&slice)
            .iter()
            .any(|n| close(n.value, quantity.value));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn unit_of(name: &str) -> Unit {
        serde_json::from_value(Value::String(name.into())).unwrap_or_else(|_| panic!("unit {name}"))
    }

    fn char_span(text: &str, matched: &str, from_char: usize) -> [usize; 2] {
        let chars: Vec<char> = text.chars().collect();
        let needle: Vec<char> = matched.chars().collect();
        (from_char..=chars.len().saturating_sub(needle.len()))
            .find(|&s| chars[s..s + needle.len()] == needle[..])
            .map(|s| [s, s + needle.len()])
            .unwrap_or_else(|| panic!("{matched:?} not in {text:?}"))
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * b.abs().max(1.0)
    }

    fn check(case: &Value) {
        let text = case["text"].as_str().unwrap();
        let expect = case["expect"].as_array().unwrap();
        let got = parse_quantities(text);
        let describe = || format!("{text:?} -> {got:?}");
        assert_eq!(got.len(), expect.len(), "match count for {}", describe());
        let mut from = 0;
        for (m, e) in got.iter().zip(expect) {
            let span = char_span(text, e["matched"].as_str().unwrap(), from);
            assert_eq!(m.span, span, "span for {}", describe());
            from = span[1];
            let f = |k: &str| e[k].as_f64().unwrap();
            let unit = |k: &str| e[k].as_str().map(unit_of);
            match (e["kind"].as_str().unwrap(), &m.value) {
                ("quantity", Parsed::Quantity(q)) => {
                    assert!(close(q.value, f("value")), "value for {}", describe());
                    assert_eq!(Some(q.unit), unit("unit"), "unit for {}", describe());
                }
                ("range", Parsed::Range { lo, hi }) => {
                    assert!(
                        close(lo.value, f("lo")) && close(hi.value, f("hi")),
                        "bounds for {}",
                        describe()
                    );
                    assert_eq!(Some(lo.unit), unit("unit"), "unit for {}", describe());
                    assert_eq!(Some(hi.unit), unit("unit"), "unit for {}", describe());
                }
                (
                    "tolerance",
                    Parsed::Tolerance {
                        upper,
                        lower,
                        symmetric,
                    },
                ) => {
                    assert!(
                        close(upper.value, f("upper")) && close(lower.value, f("lower")),
                        "offsets for {}",
                        describe()
                    );
                    assert_eq!(upper.unit, unit("unit"), "unit for {}", describe());
                    assert_eq!(lower.unit, unit("unit"));
                    assert_eq!(*symmetric, e["symmetric"].as_bool().unwrap());
                }
                (
                    "thread",
                    Parsed::Thread {
                        diameter_mm,
                        pitch_mm,
                    },
                ) => {
                    assert!(
                        close(*diameter_mm, f("diameter_mm")),
                        "diameter for {}",
                        describe()
                    );
                    assert_eq!(
                        *pitch_mm,
                        e["pitch_mm"].as_f64(),
                        "pitch for {}",
                        describe()
                    );
                }
                (
                    "fit",
                    Parsed::Fit {
                        letter,
                        grade,
                        hole,
                    },
                ) => {
                    assert_eq!(letter.to_string(), e["letter"].as_str().unwrap());
                    assert_eq!(*grade as u64, e["grade"].as_u64().unwrap());
                    assert_eq!(*hole, e["hole"].as_bool().unwrap());
                }
                (kind, other) => panic!("expected {kind}, got {other:?} for {}", describe()),
            }
        }
    }

    #[test]
    fn corpus() {
        let corpus: Vec<Value> = serde_json::from_str(include_str!(
            "../../tests/fixtures/requirements-quantity-corpus.json"
        ))
        .unwrap();
        assert!(corpus.len() >= 100, "corpus must have at least 100 cases");
        let negatives = corpus
            .iter()
            .filter(|c| c["expect"].as_array().unwrap().is_empty())
            .count();
        assert!(negatives >= 30, "corpus needs negatives");
        for case in &corpus {
            check(case);
        }
    }

    fn one(text: &str) -> Parsed {
        let got = parse_quantities(text);
        assert_eq!(got.len(), 1, "{text:?} -> {got:?}");
        got[0].value.clone()
    }

    #[test]
    fn spans_are_character_offsets() {
        let m = &parse_quantities("±0.1 µm then 110°")[..];
        assert_eq!(m[0].span, [0, 7]);
        assert_eq!(m[1].span, [13, 17]);
    }

    #[test]
    fn spec_examples() {
        assert_eq!(one("8mm"), Parsed::Quantity(Quantity::new(8.0, Unit::Mm)));
        assert_eq!(one("8 mm"), Parsed::Quantity(Quantity::new(8.0, Unit::Mm)));
        assert_eq!(
            one("half a millimetre"),
            Parsed::Quantity(Quantity::new(0.5, Unit::Mm))
        );
        assert_eq!(
            one("110 deg"),
            Parsed::Quantity(Quantity::new(110.0, Unit::Deg))
        );
        assert_eq!(
            one("1e-5 mm³"),
            Parsed::Quantity(Quantity::new(1e-5, Unit::Mm3))
        );
        assert_eq!(
            one("M8"),
            Parsed::Thread {
                diameter_mm: 8.0,
                pitch_mm: None
            }
        );
        assert_eq!(
            one("M2.5"),
            Parsed::Thread {
                diameter_mm: 2.5,
                pitch_mm: None
            }
        );
        assert_eq!(
            one("H7"),
            Parsed::Fit {
                letter: 'H',
                grade: 7,
                hole: true
            }
        );
        assert_eq!(
            one("h6"),
            Parsed::Fit {
                letter: 'h',
                grade: 6,
                hole: false
            }
        );
        assert!(
            matches!(one("-5 to -3 mm"), Parsed::Range { lo, hi } if lo.value == -5.0 && hi.value == -3.0)
        );
    }

    #[test]
    fn identifiers_are_not_fit_classes() {
        for text in [
            "E1",
            "R2 and E4",
            "case E6 holds",
            "Step P5",
            "E5: lid fits",
            "see (E5)",
            "Req R6",
        ] {
            assert!(parse_quantities(text).is_empty(), "{text:?}");
        }
        assert_eq!(
            one("E2 mm gap H7"),
            Parsed::Fit {
                letter: 'H',
                grade: 7,
                hole: true
            }
        );
        assert_eq!(
            one("bore E9"),
            Parsed::Fit {
                letter: 'E',
                grade: 9,
                hole: true
            }
        );
    }

    #[test]
    fn token_check_is_case_insensitive_for_threads_only() {
        let t = "ensure slab attaches to beam cleanly using m8s";
        let all = [0, t.chars().count()];
        assert!(token_in_span(t, all, "M8"));
        assert!(token_in_span("use M8 bolts", [0, 12], "M8"));
        assert!(!token_in_span("use M80 bolts", [0, 13], "M8"));
        assert!(!token_in_span("use M8x bolts", [0, 13], "M8"));
        assert!(token_in_span("fit h7 here", [0, 11], "h7"));
        assert!(!token_in_span("fit h7 here", [0, 11], "H7"));
        assert!(!token_in_span("fit H7s here", [0, 12], "H7"));
        assert!(!token_in_span("use M8 bolts", [0, 3], "M8"));
    }

    #[test]
    fn find_numbers_reports_bare_numbers() {
        let n = find_numbers("step 5 from -2.5 to 1,000 in v2 and tower_5");
        let values: Vec<f64> = n.iter().map(|t| t.value).collect();
        assert_eq!(values, [5.0, -2.5, 1000.0]);
    }

    #[test]
    fn provenance_check_finds_values_in_span() {
        let text =
            "door opens to 110° without hitting frame, gap 0.2–0.4 mm, bore 8 mm H7 +0/+15 µm";
        let find = |s: &str| {
            let a = text.find(s).unwrap();
            let start = text[..a].chars().count();
            [start, start + s.chars().count()]
        };
        let deg = Quantity::new(110.0, Unit::Deg);
        assert!(quantity_in_span(text, find("door opens to 110°"), &deg));
        assert!(quantity_in_span(
            text,
            find("door opens to 110°"),
            &Quantity::new(110f64.to_radians(), Unit::Rad)
        ));
        assert!(!quantity_in_span(text, find("without hitting frame"), &deg));
        let gap = find("gap 0.2–0.4 mm");
        assert!(quantity_in_span(text, gap, &Quantity::new(0.2, Unit::Mm)));
        assert!(quantity_in_span(text, gap, &Quantity::new(0.0004, Unit::M)));
        assert!(!quantity_in_span(text, gap, &Quantity::new(0.3, Unit::Mm)));
        assert!(quantity_in_span(
            text,
            find("+0/+15 µm"),
            &Quantity::new(0.015, Unit::Mm)
        ));
        assert!(!quantity_in_span(
            text,
            find("gap"),
            &Quantity::new(0.2, Unit::Mm)
        ));
        assert!(quantity_in_span(
            "extension 5 to 15",
            [0, 17],
            &Quantity::new(15.0, Unit::One)
        ));
    }
}

//! Coverage (spec §7) and vague-word detection.
//!
//! * `E_COVER_MENTION`: an `@` chip that no claim, allowance, set or cover
//!   uses and that isn't inside an `unsupported` span.
//! * `W_COVER_TEXT`: prose outside every claim, allowance, set and
//!   `unsupported` span. Chip tokens are excluded (their own check is above),
//!   and so are runs made only of connective words ([`CONNECTIVES`]) or
//!   punctuation.
//! * `E_VAGUE`: a table word with several readings inside a claim's prose,
//!   with no recorded answer.
use super::bind::{Bindings, Chip, glob_match};
use super::diagnostics::{Diagnostic, DiagnosticCode, Fix};
use super::ir::{PartName, Requirement};
use super::library::{find_vague, vague_table};
use super::refs::{self, Ref};

/// Words that may sit between claims without being "uncovered prose".
pub const CONNECTIVES: &[&str] = &[
    "and", "or", "the", "a", "an", "then", "also", "that", "please",
];

fn within(inner: [usize; 2], outer: [usize; 2]) -> bool {
    outer[0] <= inner[0] && inner[1] <= outer[1]
}

fn chip_used(req: &Requirement, chip: &Chip) -> bool {
    if req.unsupported.iter().any(|u| within(chip.span, u.span)) {
        return true;
    }
    if req.covers.iter().any(|c| c.model == chip.model) {
        return true;
    }
    let mut used = false;
    refs::walk(req, &mut |site| {
        let hit = |model: &super::ir::ModelRef, part: Option<&PartName>| {
            if model != &chip.model {
                return false;
            }
            match (&chip.part, part) {
                (None, _) => true,
                (Some(want), Some(PartName::Name(n))) => n == want,
                (Some(want), Some(PartName::Glob(g))) => glob_match(g, want),
                (Some(_), None) => false,
            }
        };
        used |= match site.reference {
            Ref::Model(m) => hit(m, None),
            Ref::Part(p) => hit(&p.model, Some(&p.part)),
            Ref::Param(p) => p.model.as_ref().is_some_and(|m| hit(m, None)),
            Ref::Joint(j) => hit(&j.model, None),
            Ref::Feature(f) => {
                let part = match f {
                    super::ir::FeatureRef::Explicit { part, .. }
                    | super::ir::FeatureRef::History { part, .. }
                    | super::ir::FeatureRef::Geometric { part, .. } => part,
                };
                hit(&part.model, Some(&part.part))
            }
            Ref::Set(_) | Ref::Others => false,
        };
    });
    used
}

/// `E_COVER_MENTION` and `W_COVER_TEXT`.
pub fn check_coverage(req: &Requirement, b: &Bindings) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for chip in &b.chips {
        if !chip_used(req, chip) {
            out.push(
                Diagnostic::new(
                    DiagnosticCode::ECoverMention,
                    "text",
                    format!(
                        "@{} is mentioned, but no claim checks it and it isn't marked unsupported",
                        chip.label
                    ),
                )
                .with_span(Some(chip.span)),
            );
        }
    }
    let chars: Vec<char> = req.text.chars().collect();
    let mut covered = vec![false; chars.len()];
    let mut mark = |s: [usize; 2]| {
        for c in covered.iter_mut().take(s[1].min(chars.len())).skip(s[0]) {
            *c = true;
        }
    };
    req.claims.iter().filter_map(|c| c.span).for_each(&mut mark);
    req.allow.iter().filter_map(|a| a.span).for_each(&mut mark);
    req.sets.values().filter_map(|s| s.span).for_each(&mut mark);
    req.unsupported.iter().for_each(|u| mark(u.span));
    b.chips.iter().for_each(|c| mark(c.span));
    let mut i = 0;
    while i < chars.len() {
        if covered[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !covered[i] {
            i += 1;
        }
        let run: String = chars[start..i].iter().collect();
        let words: Vec<String> = run
            .split(|c: char| !c.is_alphanumeric() && c != '\'')
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        if words.iter().all(|w| CONNECTIVES.contains(&w.as_str())) {
            continue;
        }
        let lead = run.chars().take_while(|c| !c.is_alphanumeric()).count();
        let trail = run
            .chars()
            .rev()
            .take_while(|c| !c.is_alphanumeric())
            .count();
        let span = [start + lead, i - trail];
        let shown: String = chars[span[0]..span[1]].iter().collect();
        out.push(
            Diagnostic::new(
                DiagnosticCode::WCoverText,
                "text",
                format!("\"{shown}\" is not covered by any claim or marked unsupported"),
            )
            .with_span(Some(span)),
        );
    }
    out
}

/// `E_VAGUE` for ambiguous table words used by a claim and not yet answered.
pub fn check_vague(req: &Requirement) -> Vec<Diagnostic> {
    let spans: Vec<[usize; 2]> = req
        .claims
        .iter()
        .filter_map(|c| c.span)
        .chain(req.allow.iter().filter_map(|a| a.span))
        .collect();
    let mut out = Vec::new();
    for m in find_vague(&req.text) {
        if !m.ambiguous || !spans.iter().any(|s| within(m.span, *s)) {
            continue;
        }
        if req
            .answers
            .iter()
            .any(|a| a.phrase.eq_ignore_ascii_case(m.phrase))
        {
            continue;
        }
        let entry = vague_table()
            .iter()
            .find(|e| e.phrase == m.phrase)
            .expect("table word");
        out.push(
            Diagnostic::new(
                DiagnosticCode::EVague,
                "text",
                format!("\"{}\" can mean more than one thing", m.phrase),
            )
            .with_span(Some(m.span))
            .with_fix(Fix::AskUser {
                options: entry
                    .readings
                    .iter()
                    .map(|r| r.summary.to_string())
                    .collect(),
                question: Some(entry.question.to_string()),
            }),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::requirements::bind::parse_chips;
    use serde_json::json;

    fn req(text: &str, rest: serde_json::Value) -> (Requirement, Bindings) {
        let mut v = json!({"ir_version": 1, "id": "r", "name": "n", "text": text, "claims": []});
        for (k, x) in rest.as_object().unwrap() {
            v[k] = x.clone();
        }
        let r: Requirement = serde_json::from_value(v).unwrap();
        let b = Bindings {
            chips: parse_chips(text),
            ..Default::default()
        };
        (r, b)
    }

    fn part(model: &str, name: &str) -> serde_json::Value {
        json!({"part": {"model": {"entry": format!("{model}.py"), "builder": model}, "part": {"name": name}}})
    }

    #[test]
    fn unused_chips_are_errors_used_chips_are_not() {
        let text = "@[Lid](lid.py#lid) fits @[Box](box.py#box) and @[Pin](pin.py#pin)";
        let (r, b) = req(
            text,
            json!({"claims": [
            {"id": "c1", "rule": "no_overlap", "span": [0, 66], "args": {"a": part("lid", "x"), "b": part("box", "y")}}]}),
        );
        let d = check_coverage(&r, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, DiagnosticCode::ECoverMention);
        assert_eq!(d[0].span, Some(b.chips[2].span));
    }

    #[test]
    fn chips_count_as_used_through_parts_globs_covers_and_unsupported() {
        let text = "@[A](m.py#m/tower_1) @[B](m.py#m/door) @[C](c.py#c) @[D](d.py#d) ok";
        let (r, b) = req(
            text,
            json!({
            "claims": [{"id": "c1", "rule": "x", "span": [0, 5], "args": {"a": {"part": {"model": {"entry": "m.py", "builder": "m"}, "part": {"glob": "tower_*"}}}}}],
            "covers": [{"model": {"entry": "c.py", "builder": "c"}, "part": part("asm", "c")["part"].clone()}],
            "unsupported": [{"span": [52, 64], "reason": "no"}]}),
        );
        let d = check_coverage(&r, &b);
        let labels: Vec<_> = d
            .iter()
            .filter(|x| x.code == DiagnosticCode::ECoverMention)
            .map(|x| x.message.clone())
            .collect();
        assert_eq!(labels.len(), 1, "{labels:?}"); // only B (door) unused: the glob doesn't match it
        assert!(labels[0].contains("@B"));
    }

    #[test]
    fn uncovered_prose_warns_but_connectives_and_punctuation_do_not() {
        let text = "door opens, and frame stays, plus something odd.";
        let (r, b) = req(
            text,
            json!({"claims": [
            {"id": "c1", "rule": "x", "span": [0, 10], "args": {}},
            {"id": "c2", "rule": "x", "span": [15, 27], "args": {}}]}),
        );
        let d = check_coverage(&r, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].code, DiagnosticCode::WCoverText);
        let [s, e] = d[0].span.unwrap();
        let shown: String = text.chars().skip(s).take(e - s).collect();
        assert_eq!(shown, "plus something odd");
        let (r, b) = req(
            text,
            json!({"claims": [{"id": "c1", "rule": "x", "span": [0, 48], "args": {}}]}),
        );
        assert!(check_coverage(&r, &b).is_empty());
    }

    #[test]
    fn unsupported_spans_cover_prose() {
        let text = "door opens and looks neat";
        let (r, b) = req(
            text,
            json!({
            "claims": [{"id": "c1", "rule": "x", "span": [0, 10], "args": {}}],
            "unsupported": [{"span": [15, 25], "reason": "judgement"}]}),
        );
        assert!(check_coverage(&r, &b).is_empty());
    }

    #[test]
    fn vague_words_ask_once_and_then_stay_quiet() {
        let text = "clamps slide freely and the box fits";
        let claims = json!([{"id": "c1", "rule": "x", "span": [0, 36], "args": {}}]);
        let (r, _) = req(text, json!({"claims": claims.clone()}));
        let d = check_vague(&r);
        let words: Vec<_> = d.iter().map(|x| x.message.as_str()).collect();
        assert_eq!(
            words,
            [
                "\"freely\" can mean more than one thing",
                "\"fits\" can mean more than one thing"
            ]
        );
        assert!(matches!(&d[0].fix, Some(Fix::AskUser { options, .. }) if options.len() == 2));
        let (r, _) = req(
            text,
            json!({"claims": claims, "answers": [
            {"phrase": "Freely", "answer": "no overlap"}, {"phrase": "fits", "answer": "inside"}]}),
        );
        assert!(check_vague(&r).is_empty());
        // Outside every claim span: not this requirement's business.
        let (r, _) = req(
            text,
            json!({"claims": [{"id": "c1", "rule": "x", "span": [0, 6], "args": {}}]}),
        );
        assert!(check_vague(&r).is_empty());
    }
}

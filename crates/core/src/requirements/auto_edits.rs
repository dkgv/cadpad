//! Auto-applied naming/metadata edits (spec §2.2, P2.23).
//!
//! The assistant proposes source edits that only add names or metadata —
//! publish a `features=` name for an unnamed face group, write fastener
//! metadata into `show(...)`. Such an edit may be applied without review,
//! but **only** when a rebuild proves every part's geometry hash is
//! unchanged. If geometry moved, the same edit is just a normal code change
//! and goes through the ordinary review path.
//!
//! This module is the pure decision layer plus the `Fix::EditSource`
//! constructors that produce the proposals. Applying the edit itself is the
//! caller's job (the agent's `edit_source` flow or a future tool command):
//! snapshot the `get_project` reply before and after, then call
//! [`classify_edit`]. Nothing here decides silently — [`classify_edit`]
//! returns [`AutoEdit::NeedsReview`] with the reason whenever the guard
//! cannot be proven.
use super::diagnostics::Fix;
use super::snapshot::SourceInfo;
use crate::geometry::FastenerMetadata;
use serde_json::Value;
use std::collections::BTreeMap;

/// What to do with a proposed source edit once the rebuild is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AutoEdit {
    /// Every part's geometry hash is unchanged: apply as a naming/metadata
    /// edit without review.
    Apply,
    /// Geometry changed (or could not be verified): present as a normal code
    /// change. The string says why.
    NeedsReview(String),
}

impl AutoEdit {
    pub fn is_apply(&self) -> bool {
        matches!(self, AutoEdit::Apply)
    }
    /// JSON form recorded on a build job (`{"decision": "apply"}` or
    /// `{"decision": "needs_review", "reason": "..."}`).
    pub fn value(&self) -> serde_json::Value {
        match self {
            AutoEdit::Apply => serde_json::json!({"decision": "apply"}),
            AutoEdit::NeedsReview(reason) => {
                serde_json::json!({"decision": "needs_review", "reason": reason})
            }
        }
    }
}

/// `instance_id → geometry fingerprint` for every part of a `get_project`
/// reply. Parts without a verified fingerprint (legacy revisions, shells
/// without solids) map to an empty string.
pub fn geometry_hashes(project: &Value) -> BTreeMap<String, String> {
    project["model"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| {
            let id = part["instance_id"]
                .as_str()
                .or_else(|| part["name"].as_str())?
                .to_string();
            let hash = part["geometry_fingerprint"]["sha256"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            Some((id, hash))
        })
        .collect()
}

/// The P2.23 guard: an edit may be auto-applied only when the rebuilt part
/// set is identical and **every** part's geometry hash is unchanged and
/// non-empty. An empty fingerprint cannot prove "unchanged", so it needs
/// review rather than a silent apply.
pub fn classify_edit(before: &Value, after: &Value) -> AutoEdit {
    let before = match verified_geometry_hashes(before) {
        Ok(parts) => parts,
        Err(reason) => return AutoEdit::NeedsReview(format!("before build: {reason}")),
    };
    let after = match verified_geometry_hashes(after) {
        Ok(parts) => parts,
        Err(reason) => return AutoEdit::NeedsReview(format!("after build: {reason}")),
    };
    if before.is_empty() || after.is_empty() {
        return AutoEdit::NeedsReview("the build published no parts to verify".into());
    }
    if before.len() != after.len() || before.keys().any(|k| !after.contains_key(k)) {
        return AutoEdit::NeedsReview(
            "the edit changed which parts are published, not just names or metadata".into(),
        );
    }
    for (part, hash) in &before {
        if hash.is_empty() {
            return AutoEdit::NeedsReview(format!(
                "{part} has no verified geometry fingerprint to compare"
            ));
        }
        if after[part] != *hash {
            return AutoEdit::NeedsReview(format!(
                "{part}'s geometry changed; the edit is not naming/metadata only"
            ));
        }
    }
    AutoEdit::Apply
}

/// Unlike the reporting helper above, the apply guard must reject incomplete
/// snapshots. Silently dropping a malformed part or duplicate instance ID
/// could otherwise make a changed part set look like a metadata-only edit.
fn verified_geometry_hashes(project: &Value) -> Result<BTreeMap<String, String>, String> {
    let parts = project["model"]["parts"]
        .as_array()
        .ok_or_else(|| "the project has no verifiable parts list".to_string())?;
    let mut hashes = BTreeMap::new();
    for (index, part) in parts.iter().enumerate() {
        let id = part["instance_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .or_else(|| part["name"].as_str().filter(|name| !name.is_empty()))
            .ok_or_else(|| format!("part at index {index} has no stable identity"))?;
        let hash = part["geometry_fingerprint"]["sha256"]
            .as_str()
            .filter(|hash| !hash.is_empty())
            .ok_or_else(|| format!("{id} has no verified geometry fingerprint"))?;
        if hashes.insert(id.to_string(), hash.to_string()).is_some() {
            return Err(format!(
                "duplicate part identity {id:?} prevents verification"
            ));
        }
    }
    if hashes.is_empty() {
        return Err("the build published no parts to verify".into());
    }
    Ok(hashes)
}

// ------------------------------------------------------- fix constructors

/// Python literal for the `fastener=` argument of `show(...)`, e.g.
/// `fastener=dict(kind="bolt", size="M8", length=20.0, mates=["frame"])`.
/// Values are quoted as strings except `length` (a number, mm) and `mates`.
pub fn fastener_literal(metadata: &FastenerMetadata) -> String {
    let mut fields = vec![
        format!("kind={:?}", metadata.kind),
        format!("size={:?}", metadata.size),
    ];
    if let Some(length) = metadata.length {
        fields.push(format!("length={length}"));
    }
    if let Some(mates) = &metadata.mates {
        let list = mates
            .iter()
            .map(|m| format!("{m:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        fields.push(format!("mates=[{list}]"));
    }
    format!("fastener=dict({})", fields.join(", "))
}

/// `Fix::EditSource` proposing explicit fastener metadata for a published
/// part — the `E_BIND_FASTENER_MISSING` remedy (spec §3.6/P3.7), with the
/// suggested `fastener=` literal filled in so the agent can apply it
/// verbatim. `metadata` is what the requirement/binding already knows
/// (kind, size); it is metadata the author supplies, never inferred from
/// geometry.
pub fn fastener_metadata_fix(
    source: Option<&SourceInfo>,
    part: &str,
    metadata: &FastenerMetadata,
) -> Fix {
    let location = source
        .map(|s| format!("{}:{}", s.file, s.line))
        .unwrap_or_else(|| part.to_string());
    Fix::EditSource {
        description: format!(
            "Publish explicit fastener metadata for {part} at {location}: add {} to its show() call. Cadpad will not infer fastener metadata from geometry or labels.",
            fastener_literal(metadata)
        ),
        path: source.map(|s| s.file.clone()),
        proposed: Some(fastener_literal(metadata)),
    }
}

/// `Fix::EditSource` proposing an explicit `features=` name for a feature
/// that currently only resolves through a positional or fragile history
/// name (`W_POSITIONAL_NAME`). The selector itself is the author's choice —
/// e.g. `features={{"{feature}": <selector>}}` inside the part's `show()` —
/// so only the description and file are filled, never invented geometry.
pub fn feature_naming_fix(
    source: Option<&SourceInfo>,
    part: &str,
    feature: &str,
    entity: &str,
) -> Fix {
    let location = source
        .map(|s| format!("{}:{}", s.file, s.line))
        .unwrap_or_else(|| part.to_string());
    Fix::EditSource {
        description: format!(
            "Name the {entity} feature {feature:?} of {part} explicitly at {location}: add it to the part's show() features map, e.g. features={{\"{feature}\": <selector>}}, so references survive edits that shift history positions."
        ),
        path: source.map(|s| s.file.clone()),
        proposed: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_guard_serializes_for_build_results() {
        assert_eq!(AutoEdit::Apply.value(), json!({"decision": "apply"}));
        assert_eq!(
            AutoEdit::NeedsReview("lid moved".into()).value(),
            json!({"decision": "needs_review", "reason": "lid moved"})
        );
        assert!(AutoEdit::Apply.is_apply());
    }

    fn project(parts: &[(&str, &str)]) -> Value {
        json!({"model": {"parts": parts.iter().map(|(id, hash)| json!({
            "instance_id": id, "name": id,
            "geometry_fingerprint": {"format": "x", "sha256": hash}
        })).collect::<Vec<_>>()}})
    }

    #[test]
    fn geometry_hashes_key_by_instance_or_name() {
        let p = json!({"model": {"parts": [
            {"instance_id": "a", "geometry_fingerprint": {"sha256": "h1"}},
            {"name": "b", "geometry_fingerprint": {"sha256": "h2"}},
            {"name": "c"}
        ]}});
        let hashes = geometry_hashes(&p);
        assert_eq!(hashes["a"], "h1");
        assert_eq!(hashes["b"], "h2");
        assert_eq!(hashes["c"], "");
    }

    #[test]
    fn metadata_only_edit_applies() {
        let before = project(&[("base", "h1"), ("lid", "h2")]);
        let after = project(&[("base", "h1"), ("lid", "h2")]);
        assert_eq!(classify_edit(&before, &after), AutoEdit::Apply);
    }

    #[test]
    fn changed_geometry_needs_review() {
        let before = project(&[("base", "h1"), ("lid", "h2")]);
        let after = project(&[("base", "h1"), ("lid", "h3")]);
        match classify_edit(&before, &after) {
            AutoEdit::NeedsReview(reason) => {
                assert!(reason.contains("lid"), "{reason}");
            }
            AutoEdit::Apply => panic!("geometry change must not auto-apply"),
        }
    }

    #[test]
    fn added_removed_or_unverifiable_parts_need_review() {
        let before = project(&[("base", "h1"), ("lid", "h2")]);
        let added = project(&[("base", "h1"), ("lid", "h2"), ("knob", "h3")]);
        assert!(matches!(
            classify_edit(&before, &added),
            AutoEdit::NeedsReview(_)
        ));
        let removed = project(&[("base", "h1")]);
        assert!(matches!(
            classify_edit(&before, &removed),
            AutoEdit::NeedsReview(_)
        ));
        let unverifiable = project(&[("base", ""), ("lid", "h2")]);
        match classify_edit(&before, &unverifiable) {
            AutoEdit::NeedsReview(reason) => assert!(reason.contains("base")),
            AutoEdit::Apply => panic!("empty fingerprint cannot prove unchanged"),
        }
        assert!(matches!(
            classify_edit(&json!({}), &before),
            AutoEdit::NeedsReview(_)
        ));
    }

    #[test]
    fn incomplete_or_ambiguous_part_snapshots_never_auto_apply() {
        let before = project(&[("base", "h1"), ("lid", "h2")]);
        let missing_identity = json!({"model": {"parts": [
            {"geometry_fingerprint": {"sha256": "h1"}},
            {"instance_id": "lid", "geometry_fingerprint": {"sha256": "h2"}}
        ]}});
        assert!(matches!(
            classify_edit(&before, &missing_identity),
            AutoEdit::NeedsReview(reason) if reason.contains("stable identity")
        ));

        let duplicate_identity = json!({"model": {"parts": [
            {"instance_id": "base", "geometry_fingerprint": {"sha256": "h1"}},
            {"instance_id": "base", "geometry_fingerprint": {"sha256": "h2"}}
        ]}});
        assert!(matches!(
            classify_edit(&duplicate_identity, &duplicate_identity),
            AutoEdit::NeedsReview(reason) if reason.contains("duplicate")
        ));

        let missing_fingerprint = json!({"model": {"parts": [
            {"instance_id": "base", "geometry_fingerprint": {"sha256": "h1"}},
            {"instance_id": "lid"}
        ]}});
        assert!(matches!(
            classify_edit(&before, &missing_fingerprint),
            AutoEdit::NeedsReview(reason) if reason.contains("fingerprint")
        ));
    }

    #[test]
    fn fastener_literal_and_fix() {
        let metadata = FastenerMetadata {
            kind: "bolt".into(),
            size: "M8".into(),
            length: Some(20.0),
            mates: Some(vec!["frame".into(), "lid".into()]),
        };
        assert_eq!(
            fastener_literal(&metadata),
            "fastener=dict(kind=\"bolt\", size=\"M8\", length=20, mates=[\"frame\", \"lid\"])"
        );
        let minimal = FastenerMetadata {
            kind: "nut".into(),
            size: "M6".into(),
            length: None,
            mates: None,
        };
        assert_eq!(
            fastener_literal(&minimal),
            "fastener=dict(kind=\"nut\", size=\"M6\")"
        );
        let source = SourceInfo {
            file: "hardware.py".into(),
            line: 27,
            function: None,
        };
        match fastener_metadata_fix(Some(&source), "bolt_1", &metadata) {
            Fix::EditSource {
                description,
                path,
                proposed,
            } => {
                assert_eq!(path.as_deref(), Some("hardware.py"));
                assert_eq!(
                    proposed.as_deref(),
                    Some(fastener_literal(&metadata).as_str())
                );
                assert!(description.contains("hardware.py:27"));
                assert!(description.contains("will not infer"));
            }
            other => panic!("expected edit_source, got {other:?}"),
        }
    }

    #[test]
    fn feature_naming_fix_describes_without_inventing_geometry() {
        let source = SourceInfo {
            file: "door.py".into(),
            line: 12,
            function: Some("make_door".into()),
        };
        match feature_naming_fix(Some(&source), "door", "hinge", "face") {
            Fix::EditSource {
                description,
                path,
                proposed,
            } => {
                assert_eq!(path.as_deref(), Some("door.py"));
                assert_eq!(proposed, None);
                assert!(description.contains("door.py:12"));
                assert!(description.contains("hinge"));
                assert!(description.contains("features="));
            }
            other => panic!("expected edit_source, got {other:?}"),
        }
        // No source location: description still names the part.
        match feature_naming_fix(None, "door", "hinge", "edge") {
            Fix::EditSource { path, .. } => assert_eq!(path, None),
            _ => unreachable!(),
        }
    }
}

//! Geometry the user selected in the viewport and attached to a prompt.
//!
//! A reference is a revision-scoped entity. Selection order assigns letters
//! (A, B, …) that match the viewport badges and the attached screenshot, so the
//! user can write "make A flush with B". Each reference resolves to exact
//! inspected properties plus the component source file whose `show()` call
//! published the part. Stale or unknown entities are request errors; they are
//! never relocated to a different face or edge.
use crate::runner::ToolExecutor;
use cadpad_core::{Error, Result, tools::Entity};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const MAX_REFERENCES: usize = 8;
pub const MAX_FOLDERS: usize = 8;
const MAX_FOLDER_ENTRIES: usize = 200;
const MAX_FOLDER_BYTES: usize = 32 * 1024;
/// Base64 PNG bytes; the runner resends the image every round under a 2 MiB context cap.
pub const MAX_IMAGE_BYTES: usize = 1024 * 1024;
const MAX_FILE_BYTES: usize = 48 * 1024;
const MAX_SOURCE_BYTES: usize = 96 * 1024;

/// What the composer sends with a prompt.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Attachments {
    #[serde(default)]
    pub references: Vec<Entity>,
    /// Base64 PNG: either the lettered viewport capture or a user-dropped reference image.
    #[serde(default)]
    pub image: Option<String>,
    /// Project-relative folders the user referenced from the Explorer.
    #[serde(default)]
    pub folders: Vec<String>,
}

/// The chip stored with a user chat message, so history shows what was referenced.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatReference {
    pub letter: String,
    pub instance_id: String,
    pub entity_id: String,
    pub part: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
}

/// Context resolved at turn start, ready for either backend.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    pub text: String,
    pub image: Option<String>,
}

pub fn letter(index: usize) -> String {
    let mut text = String::new();
    let mut n = index as i64;
    loop {
        text.insert(0, (b'A' + (n % 26) as u8) as char);
        n = n / 26 - 1;
        if n < 0 {
            return text;
        }
    }
}

fn kind_label(kind: &str) -> &'static str {
    match kind {
        "face" => "Face",
        "edge" => "Edge",
        _ => "Whole part",
    }
}

/// Validates entities against the committed project and builds the chips.
pub fn summarize(project: &Value, entities: &[Entity]) -> Result<Vec<ChatReference>> {
    if entities.len() > MAX_REFERENCES {
        return Err(Error::domain(
            "INVALID_REQUEST",
            format!("Attach at most {MAX_REFERENCES} references"),
        ));
    }
    let revision = project["revision"].as_str().unwrap_or_default();
    let parts = project["model"]["parts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut seen = BTreeSet::new();
    let mut chips = Vec::with_capacity(entities.len());
    for (index, entity) in entities.iter().enumerate() {
        if entity.revision != revision {
            return Err(Error::domain(
                "STALE_ENTITY",
                "The model was rebuilt after you selected this geometry; select it again",
            ));
        }
        if !seen.insert((entity.instance_id.clone(), entity.entity_id.clone())) {
            return Err(Error::domain("INVALID_REQUEST", "Duplicate reference"));
        }
        let part = parts
            .iter()
            .find(|part| part["instance_id"] == entity.instance_id)
            .ok_or_else(|| Error::domain("STALE_ENTITY", "Referenced part no longer exists"))?;
        let (kind, number) = entity
            .entity_id
            .split_once(':')
            .ok_or_else(|| Error::domain("STALE_ENTITY", "Invalid referenced entity"))?;
        let number = number
            .parse::<u64>()
            .map_err(|_| Error::domain("STALE_ENTITY", "Invalid referenced topology index"))?;
        let valid = match kind {
            "part" => number == 0,
            "face" => number < part["face_count"].as_u64().unwrap_or(0),
            "edge" => number < part["edge_count"].as_u64().unwrap_or(0),
            _ => false,
        };
        if !valid {
            return Err(Error::domain(
                "STALE_ENTITY",
                "Referenced geometry does not belong to this revision",
            ));
        }
        let features = part["features"]
            .as_object()
            .map(|features| {
                features
                    .iter()
                    .filter(|(_, ids)| {
                        ids.as_array()
                            .is_some_and(|ids| ids.iter().any(|id| id == &entity.entity_id))
                    })
                    .map(|(name, _)| name.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let label = if kind == "face" && part["accuracy"] == "mesh" {
            "Mesh surface".into()
        } else if features.is_empty() {
            kind_label(kind).to_string()
        } else {
            features.join(", ")
        };
        chips.push(ChatReference {
            letter: letter(index),
            instance_id: entity.instance_id.clone(),
            entity_id: entity.entity_id.clone(),
            part: part["name"]
                .as_str()
                .unwrap_or(&entity.instance_id)
                .to_string(),
            label,
            file: part["source"]["file"].as_str().map(str::to_string),
            line: part["source"]["line"].as_u64(),
        });
    }
    Ok(chips)
}

pub fn validate_image(image: &Option<String>) -> Result<()> {
    if let Some(image) = image
        && (image.len() > MAX_IMAGE_BYTES
            || !image
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')))
    {
        return Err(Error::domain(
            "INVALID_REQUEST",
            "The attached image must be a base64 PNG up to 1 MiB",
        ));
    }
    Ok(())
}

/// One line for history, so later turns still know what an earlier message meant.
pub fn history_note(chips: &[ChatReference]) -> String {
    let items = chips
        .iter()
        .map(|chip| {
            format!(
                "{}={} {} ({}{})",
                chip.letter,
                chip.part,
                chip.label.to_lowercase(),
                chip.entity_id,
                chip.file
                    .as_ref()
                    .map(|file| format!(", {file}"))
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("[Referenced geometry, earlier revision: {items}]")
}

/// Normalizes Explorer folder paths: project-relative, `/`-separated, no
/// `.`/`..` segments. Duplicates are dropped; order is kept.
pub fn validate_folders(folders: &[String]) -> Result<Vec<String>> {
    let mut seen = BTreeSet::new();
    let mut valid = Vec::with_capacity(folders.len());
    for folder in folders {
        let path = folder.trim().replace('\\', "/");
        let path = path.trim_end_matches('/');
        if path.is_empty()
            || path.starts_with('/')
            || path.len() > 512
            || path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
        {
            return Err(Error::domain(
                "INVALID_REQUEST",
                "Referenced folders must be project-relative paths",
            ));
        }
        if seen.insert(path.to_string()) {
            valid.push(path.to_string());
        }
    }
    if valid.len() > MAX_FOLDERS {
        return Err(Error::domain(
            "INVALID_REQUEST",
            format!("Attach at most {MAX_FOLDERS} folders"),
        ));
    }
    Ok(valid)
}

/// One line for history, so later turns still know which folders were meant.
pub fn folder_note(folders: &[String]) -> String {
    format!("[Referenced folders: {}]", folders.join(", "))
}

/// Describes each referenced folder from the project source catalog
/// (`{"files":[{"path","description","builders":[...]}]}`), bounded so a large
/// tree cannot crowd out the conversation.
pub fn folder_context(folders: &[String], catalog: Result<Value>) -> String {
    let mut text = String::from(
        "The user referenced these project folders in the Explorer and attached them to the message below. \
Paths are relative to the project root; use read_source, search_source and find_symbol on the listed files. \
\"this folder\" means the folder below when only one is listed.\n<folders>\n",
    );
    let files = match catalog {
        Ok(catalog) => catalog["files"].as_array().cloned().unwrap_or_default(),
        Err(error) => {
            for folder in folders {
                text.push_str(&format!(
                    "<folder path=\"{folder}\" unavailable=\"{}\"/>\n",
                    error.to_string().replace('"', "'")
                ));
            }
            text.push_str("</folders>\n");
            return text;
        }
    };
    let mut budget = MAX_FOLDER_BYTES;
    for folder in folders {
        let prefix = format!("{folder}/");
        let mut inside = files
            .iter()
            .filter(|file| {
                file["path"]
                    .as_str()
                    .is_some_and(|path| path.starts_with(&prefix))
            })
            .collect::<Vec<_>>();
        inside.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        if inside.is_empty() {
            text.push_str(&format!(
                "<folder path=\"{folder}\" missing=\"no project files under this path\"/>\n"
            ));
            continue;
        }
        text.push_str(&format!(
            "<folder path=\"{folder}\" files=\"{}\">\n",
            inside.len()
        ));
        let mut shown = 0;
        for file in &inside {
            let path = file["path"].as_str().unwrap_or_default();
            let models = file["builders"]
                .as_array()
                .map(|builders| {
                    builders
                        .iter()
                        .filter(|builder| builder["available"] == true)
                        .filter_map(|builder| {
                            let label = builder["label"].as_str()?;
                            Some(match builder["kind"].as_str() {
                                Some(kind) => format!("{label} ({kind})"),
                                None => label.to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let description = file["description"].as_str().unwrap_or_default().trim();
            let mut line = format!("- {path}");
            if !description.is_empty() {
                line.push_str(&format!(" — {}", truncate(description, 200).0));
            }
            if !models.is_empty() {
                line.push_str(&format!(" [entries: {}]", models.join(", ")));
            }
            line.push('\n');
            if shown >= MAX_FOLDER_ENTRIES || line.len() > budget {
                break;
            }
            budget -= line.len();
            shown += 1;
            text.push_str(&line);
        }
        if shown < inside.len() {
            text.push_str(&format!(
                "- … {} more files omitted for context budget\n",
                inside.len() - shown
            ));
        }
        text.push_str("</folder>\n");
    }
    text.push_str("</folders>\n");
    text
}

fn truncate(text: &str, limit: usize) -> (&str, bool) {
    if text.len() <= limit {
        return (text, false);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Inspects references and reads their component files. Exact inspection can
/// be unavailable (for example during worker recovery); that is reported in
/// the context instead of failing the user's turn.
pub struct Request<'a> {
    pub workspace: &'a str,
    pub revision: &'a str,
    pub pose_version: u64,
    pub entities: &'a [Entity],
    pub chips: &'a [ChatReference],
    pub image: Option<String>,
    /// CLI backends receive text only; they can call capture_views with single=true.
    pub image_supported: bool,
}
pub async fn resolve(tools: &dyn ToolExecutor, request: Request<'_>) -> Result<Resolved> {
    let Request {
        workspace,
        revision,
        pose_version,
        entities,
        chips,
        image,
        image_supported,
    } = request;
    if chips.is_empty() {
        let image = if image_supported { image } else { None };
        return Ok(Resolved {
            text: if image.is_some() {
                "The user attached a reference image to the message below.\n".into()
            } else {
                String::new()
            },
            image,
        });
    }
    // History names replay the build once per worker with tracing and tie each
    // face/edge to the CadQuery call that created it. Tracing can be refused
    // (mesh revisions, nondeterministic builds); plain properties still help.
    let mut inspected = tools
        .call(
            "inspect",
            json!({"workspace_id":workspace,"revision":revision,"entities":entities,"pose_version":pose_version,"history_names":true}),
        )
        .await;
    let mut trace_error = None;
    if let Err(error) = &inspected {
        trace_error = Some(error.to_string());
        inspected = tools
            .call(
                "inspect",
                json!({"workspace_id":workspace,"revision":revision,"entities":entities,"pose_version":pose_version}),
            )
            .await;
    }
    let mut properties = match inspected {
        Ok(value) => value,
        Err(error) => json!({"unavailable": error.to_string()}),
    };
    let origins = chips
        .iter()
        .map(|chip| origin(&properties, chip))
        .collect::<Vec<_>>();
    strip_history(&mut properties);
    let references = chips
        .iter()
        .zip(&origins)
        .map(|(chip, origin)| {
            let mut reference = json!({
                "letter": chip.letter,
                "part": chip.part,
                "entity": {"revision": revision, "instance_id": chip.instance_id, "entity_id": chip.entity_id},
                "label": chip.label,
                "published_by": chip.file.as_ref().map(|file| json!({"file": file, "line": chip.line})),
            });
            if let Some(origin) = origin {
                reference["created_by"] = origin.clone();
            }
            reference
        })
        .collect::<Vec<_>>();
    let mesh = properties["accuracy"] == "mesh";
    let traced = origins.iter().any(Option::is_some);
    let mut text = String::from(
        "The user selected this geometry in the viewport and attached it to the message below. \
Letters match the badges in the user's viewport screenshot; with one reference the user may say \"this\". \
Entity IDs are scoped to the stated revision and are already resolved: the properties below are exact, \
so do not re-inspect them or ask the user which feature they meant. ",
    );
    text.push_str(if mesh {
        "Properties are measured from the STL triangles. A selected surface may group adjacent coplanar triangles.\n"
    } else if traced {
        "`created_by` is traced from the build: `calls[0]` is the CadQuery operation that created the face or edge, \
followed by the caller chain (outermost last). Explain the feature from that code. `positional: true` means the name depends on call order, \
which is still exact for this revision. `published_by` is only where the part was shown.\n"
    } else {
        "`published_by` is where the part was shown; face/edge provenance could not be traced, \
so match the exact properties (world_center, normal, size) to the code that produced them.\n"
    });
    if let Some(error) = trace_error.filter(|_| !mesh && !traced) {
        text.push_str(&format!("History tracing was unavailable: {error}\n"));
    }
    text.push_str("<references>\n");
    let mut context = json!({"revision": revision, "pose_version": pose_version, "units": "mm", "references": references});
    context[if mesh {
        "mesh_properties"
    } else {
        "exact_properties"
    }] = properties;
    text.push_str(&serde_json::to_string_pretty(&context)?);
    text.push_str("\n</references>\n");
    // Creating code first: it explains the selected face; the publishing file
    // is often a generated entry point or an assembly.
    let mut files = Vec::new();
    for origin in origins.iter().flatten() {
        for call in origin["calls"].as_array().into_iter().flatten().take(2) {
            if let Some(file) = call["file"].as_str() {
                files.push(file.to_string());
            }
        }
    }
    files.extend(chips.iter().filter_map(|chip| chip.file.clone()));
    let mut seen = BTreeSet::new();
    let files = files
        .into_iter()
        .filter(|file| file.ends_with(".py") && seen.insert(file.clone()))
        .collect::<Vec<_>>();
    let mut budget = MAX_SOURCE_BYTES;
    for file in files {
        if budget == 0 {
            text.push_str(&format!(
                "<source path=\"{file}\" omitted=\"context budget\"/>\n"
            ));
            continue;
        }
        match tools
            .call(
                "read_source",
                json!({"workspace_id":workspace,"version":revision,"path":file}),
            )
            .await
        {
            Ok(value) => {
                let source = value["source"].as_str().unwrap_or_default();
                let (shown, cut) = truncate(source, MAX_FILE_BYTES.min(budget));
                budget -= shown.len();
                text.push_str(&format!(
                    "<source path=\"{file}\" revision=\"{revision}\"{}>\n{shown}\n</source>\n",
                    if cut {
                        " truncated=\"true; use search_source or paged read_source for the rest\""
                    } else {
                        ""
                    }
                ));
            }
            Err(error) => text.push_str(&format!(
                "<source path=\"{file}\" unavailable=\"{}\"/>\n",
                error.to_string().replace('"', "'")
            )),
        }
    }
    let image = if image_supported { image } else { None };
    if image.is_none() && chips.len() > 1 {
        text.push_str("No screenshot is attached. Rendered captures (capture_views) do not draw the selection badges; \
locate references by their world_center and bounds instead.\n");
    }
    Ok(Resolved { text, image })
}

/// The traced creation site of one reference: the history names and the
/// source frames of the operation that created it.
fn origin(properties: &Value, chip: &ChatReference) -> Option<Value> {
    let item = properties["entities"].as_array()?.iter().find(|item| {
        item["reference"]["instance_id"] == chip.instance_id.as_str()
            && item["reference"]["entity_id"] == chip.entity_id.as_str()
    })?;
    let names = item["history_references"].as_array()?;
    let calls = names
        .iter()
        .find_map(|name| name["source"].as_array().filter(|calls| !calls.is_empty()))?;
    Some(json!({
        "history_names": names.iter().filter_map(|name| name["name"].as_str()).collect::<Vec<_>>(),
        "positional": names.iter().any(|name| name["positional"] == true),
        "calls": calls,
    }))
}

/// History references are restated as `created_by`; drop the raw copies and
/// hashes so they do not crowd the context.
fn strip_history(properties: &mut Value) {
    if let Some(items) = properties["entities"].as_array_mut() {
        for item in items {
            if let Some(item) = item.as_object_mut() {
                item.remove("history_references");
                item.remove("history_references_status");
                item.remove("history_references_reason");
                item.remove("anchor");
                item.remove("circular_definition");
            }
        }
    }
    if let Some(properties) = properties.as_object_mut() {
        properties.remove("history_names");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Value {
        json!({"revision":"r1","model":{"parts":[{"instance_id":"plate","name":"plate","face_count":6,"edge_count":12,
            "features":{"top":["face:5"]},"source":{"file":"parts/plate.py","line":7}}]}})
    }
    fn entity(id: &str) -> Entity {
        Entity {
            revision: "r1".into(),
            instance_id: "plate".into(),
            entity_id: id.into(),
        }
    }

    #[test]
    fn letters_follow_selection_order() {
        assert_eq!(
            [0, 1, 25, 26, 27].map(letter),
            ["A", "B", "Z", "AA", "AB"].map(String::from)
        );
    }

    #[test]
    fn chips_use_semantic_names_and_component_files() {
        let chips = summarize(&project(), &[entity("face:5"), entity("edge:3")]).unwrap();
        assert_eq!(chips[0].label, "top");
        assert_eq!(chips[0].file.as_deref(), Some("parts/plate.py"));
        assert_eq!(chips[1].label, "Edge");
        assert_eq!(chips[1].letter, "B");
    }

    #[test]
    fn stale_and_out_of_range_references_are_rejected() {
        let mut stale = entity("face:1");
        stale.revision = "r0".into();
        assert_eq!(
            summarize(&project(), &[stale]).unwrap_err().code(),
            "STALE_ENTITY"
        );
        assert_eq!(
            summarize(&project(), &[entity("face:6")])
                .unwrap_err()
                .code(),
            "STALE_ENTITY"
        );
        assert_eq!(
            summarize(&project(), &[entity("face:1"), entity("face:1")])
                .unwrap_err()
                .code(),
            "INVALID_REQUEST"
        );
    }

    #[test]
    fn folders_are_project_relative_and_deduplicated() {
        assert_eq!(
            validate_folders(&["parts/".into(), "parts".into(), "a\\b".into()]).unwrap(),
            ["parts", "a/b"].map(String::from)
        );
        for bad in ["", "/etc", "../x", "a/./b", "a//b", "C:/x"] {
            assert!(validate_folders(&[bad.into()]).is_err(), "{bad}");
        }
        assert!(
            validate_folders(
                &(0..=MAX_FOLDERS)
                    .map(|i| format!("f{i}"))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
    }

    #[test]
    fn folder_context_lists_files_under_the_folder_only() {
        let catalog = json!({"files":[
            {"path":"parts/plate.py","description":"Base plate","builders":[{"label":"build","kind":"part","available":true}]},
            {"path":"parts/sub/pin.py","description":"","builders":[]},
            {"path":"partsx/other.py","description":"","builders":[]},
            {"path":"model.py","description":"","builders":[]}
        ]});
        let text = folder_context(&["parts".into(), "empty".into()], Ok(catalog));
        assert!(text.contains("- parts/plate.py — Base plate [entries: build (part)]"));
        assert!(text.contains("- parts/sub/pin.py"));
        assert!(!text.contains("partsx/other.py"));
        assert!(!text.contains("model.py"));
        assert!(text.contains("<folder path=\"empty\" missing="));
    }

    #[test]
    fn images_are_bounded_base64() {
        assert!(validate_image(&Some("iVBORw0KGgo=".into())).is_ok());
        assert!(validate_image(&Some("data:image/png;base64,xx".into())).is_err());
        assert!(validate_image(&Some("A".repeat(MAX_IMAGE_BYTES + 1))).is_err());
    }

    struct Traced {
        trace: bool,
        reads: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl ToolExecutor for Traced {
        async fn call(&self, name: &str, args: Value) -> Result<Value> {
            match name {
                "inspect" if args["history_names"] == true && !self.trace => Err(Error::domain(
                    "HISTORY_TRACE_MISMATCH",
                    "replay differed",
                )),
                "inspect" => {
                    let mut item = json!({"reference":{"instance_id":"plate","entity_id":"face:5"},
                        "kind":"face","world_center":[1,2,3],"anchor":{"kind":"geometry_center"}});
                    if args["history_names"] == true {
                        item["history_references"] = json!([{"name":"parts/tab.py:tab/box#0@frame>tab#1/left",
                            "positional":true,"exact_hash":"x","source":[
                                {"file":"parts/tab.py","line":4,"function":"tab"},
                                {"file":"parts/frame.py","line":9,"function":"frame"}]}]);
                    }
                    Ok(json!({"accuracy":"exact","entities":[item]}))
                }
                "read_source" => {
                    self.reads.lock().unwrap().push(args["path"].as_str().unwrap().into());
                    Ok(json!({"source":"def tab(): ..."}))
                }
                other => unreachable!("unexpected tool {other}"),
            }
        }
    }

    async fn resolve_traced(trace: bool) -> (Resolved, Vec<String>) {
        let tools = Traced {
            trace,
            reads: Default::default(),
        };
        let entities = [entity("face:5")];
        let chips = summarize(&project(), &entities).unwrap();
        let resolved = resolve(
            &tools,
            Request {
                workspace: "ws",
                revision: "r1",
                pose_version: 0,
                entities: &entities,
                chips: &chips,
                image: None,
                image_supported: true,
            },
        )
        .await
        .unwrap();
        (resolved, tools.reads.into_inner().unwrap())
    }

    #[tokio::test]
    async fn traced_references_point_at_the_creating_code() {
        let (resolved, reads) = resolve_traced(true).await;
        assert!(resolved.text.contains("\"created_by\""));
        assert!(resolved.text.contains("\"function\": \"tab\""));
        assert!(!resolved.text.contains("history_references"));
        assert!(!resolved.text.contains("anchor"));
        assert_eq!(reads, ["parts/tab.py", "parts/frame.py", "parts/plate.py"]);
    }

    #[tokio::test]
    async fn refused_tracing_falls_back_to_exact_properties() {
        let (resolved, reads) = resolve_traced(false).await;
        assert!(!resolved.text.contains("\"created_by\""));
        assert!(resolved.text.contains("world_center"));
        assert!(resolved.text.contains("History tracing was unavailable"));
        assert_eq!(reads, ["parts/plate.py"]);
    }

    #[tokio::test]
    async fn image_only_attachments_keep_the_image() {
        struct NoTools;
        #[async_trait::async_trait]
        impl ToolExecutor for NoTools {
            async fn call(&self, _: &str, _: Value) -> Result<Value> {
                unreachable!("image-only resolve does not call tools")
            }
        }
        let resolved = resolve(
            &NoTools,
            Request {
                workspace: "ws",
                revision: "r1",
                pose_version: 0,
                entities: &[],
                chips: &[],
                image: Some("iVBORw0KGgo=".into()),
                image_supported: true,
            },
        )
        .await
        .unwrap();
        assert!(resolved.text.contains("reference image"));
        assert_eq!(resolved.image.as_deref(), Some("iVBORw0KGgo="));
    }
}

//! Human-readable labels for tool activity. Emitters pass the tool input so a
//! step can say what a call is doing, not just which tool it uses.
use serde_json::Value;

#[derive(Default)]
pub struct Described {
    /// A more specific label than the tool's generic default.
    pub label: Option<String>,
    /// Secondary text shown under the label: a requirement name, source path.
    pub detail: Option<String>,
}

/// Describe one tool call. Unknown tools and inputs keep the default label.
pub fn describe(tool: &str, input: Option<&Value>) -> Described {
    match tool {
        "control_requirements" => requirement(input),
        "read_source" => read_source(input),
        "update_source" => update_source(input),
        "inspect" => inspect(input),
        _ => Described::default(),
    }
}

fn requirement(input: Option<&Value>) -> Described {
    let command = input.and_then(|input| input.get("command"));
    let action = command
        .and_then(|command| command.get("action"))
        .and_then(Value::as_str);
    let label = match action {
        Some("list") => "Reading requirements",
        Some("library") => "Reading the requirement library",
        Some("schema") => "Reading the requirement schema",
        Some("compile") => "Compiling a requirement",
        Some("save") => "Saving a requirement draft",
        Some("run" | "start_run") => "Running a requirement",
        Some("cancel_run") => "Cancelling a requirement run",
        _ => "Working on requirements",
    };
    let detail = match action {
        Some("compile" | "save") => command
            .and_then(|command| command.get("ir"))
            .and_then(requirement_summary),
        _ => None,
    };
    Described {
        label: Some(label.into()),
        detail,
    }
}

fn requirement_summary(ir: &Value) -> Option<String> {
    let name = ir
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let claims = ir
        .get("claims")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut parts = Vec::new();
    if let Some(name) = name {
        parts.push(clamp(name, 64));
    }
    parts.push(match claims {
        1 => "1 claim".into(),
        count => format!("{count} claims"),
    });
    Some(parts.join(" · "))
}

fn read_source(input: Option<&Value>) -> Described {
    let Some(input) = input else {
        return Described::default();
    };
    let Some(path) = input.get("path").and_then(Value::as_str) else {
        return Described::default();
    };
    let mut detail = clamp(path, 96);
    if let Some(line) = input.get("start_line").and_then(Value::as_u64) {
        detail.push_str(&format!(" · from line {line}"));
    } else if input.get("paged").and_then(Value::as_bool) == Some(true) {
        detail.push_str(" · first page");
    }
    Described {
        label: None,
        detail: Some(detail),
    }
}

fn update_source(input: Option<&Value>) -> Described {
    let count = input
        .and_then(|input| input.get("changes"))
        .and_then(Value::as_object)
        .map(|changes| changes.len())
        .unwrap_or(0);
    if count == 0 {
        return Described::default();
    }
    Described {
        label: None,
        detail: Some(match count {
            1 => "1 file".into(),
            count => format!("{count} files"),
        }),
    }
}

fn inspect(input: Option<&Value>) -> Described {
    let count = input
        .and_then(|input| input.get("entities"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    if count == 0 {
        return Described::default();
    }
    Described {
        label: None,
        detail: Some(match count {
            1 => "1 entity".into(),
            count => format!("{count} entities"),
        }),
    }
}

fn clamp(text: &str, max: usize) -> String {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.chars().count() <= max {
        return clean;
    }
    let mut out = clean
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requirement_actions_become_specific_labels() {
        let compile = json!({
            "command": {
                "action": "compile",
                "ir": {"name": "Frame attachment", "claims": [{}, {}]}
            }
        });
        let described = describe("control_requirements", Some(&compile));
        assert_eq!(described.label.as_deref(), Some("Compiling a requirement"));
        assert_eq!(
            described.detail.as_deref(),
            Some("Frame attachment · 2 claims")
        );

        let list = json!({"command": {"action": "list"}});
        assert_eq!(
            describe("control_requirements", Some(&list))
                .label
                .as_deref(),
            Some("Reading requirements")
        );
    }

    #[test]
    fn malformed_or_unknown_commands_keep_the_default() {
        let described = describe("control_requirements", Some(&json!({})));
        assert_eq!(described.label.as_deref(), Some("Working on requirements"));
        assert!(described.detail.is_none());
    }

    #[test]
    fn source_paths_and_counts_are_bounded_details() {
        let described = describe(
            "read_source",
            Some(&json!({"path": "parts/frame.py", "start_line": 12})),
        );
        assert!(described.label.is_none());
        assert_eq!(
            described.detail.as_deref(),
            Some("parts/frame.py · from line 12")
        );

        let long = "x".repeat(200);
        let described = describe("read_source", Some(&json!({"path": long})));
        assert!(described.detail.unwrap().chars().count() <= 96);
    }
}

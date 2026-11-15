//! Working tree status for the project explorer. Paths stay relative to the
//! opened project folder, even when that folder sits inside a larger repo.
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct GitEntry {
    path: String,
    change: GitChange,
    directory: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum GitChange {
    Modified,
    Added,
    Untracked,
    Deleted,
    Conflict,
}

#[derive(Serialize)]
struct GitStatus {
    repo: bool,
    entries: Vec<GitEntry>,
}

pub fn empty() -> Value {
    json!({"repo": false, "entries": []})
}

pub fn project_status(directory: &Path) -> Value {
    let Some(prefix) = repo_prefix(directory) else {
        return empty();
    };
    let Some(output) = git(
        directory,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
        ],
    ) else {
        return json!({"repo": true, "entries": []});
    };
    let entries = scope(parse_porcelain(&output.stdout), &prefix);
    serde_json::to_value(GitStatus {
        repo: true,
        entries,
    })
    .unwrap_or_else(|_| empty())
}

fn repo_prefix(directory: &Path) -> Option<String> {
    let output = git(directory, &["rev-parse", "--show-toplevel"])?;
    let toplevel = String::from_utf8(output.stdout).ok()?;
    let root = PathBuf::from(toplevel.trim()).canonicalize().ok()?;
    let project = directory.canonicalize().ok()?;
    let relative = project.strip_prefix(&root).ok()?;
    Some(
        relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn git(directory: &Path, args: &[&str]) -> Option<std::process::Output> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args([
            "-c",
            "status.relativePaths=false",
            "-c",
            "core.quotepath=false",
        ])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    output.status.success().then_some(output)
}

fn parse_porcelain(bytes: &[u8]) -> Vec<GitEntry> {
    let mut entries = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let Some(split) = rest.iter().position(|byte| *byte == 0) else {
            break;
        };
        let record = &rest[..split];
        rest = &rest[split + 1..];
        if record.len() < 4 || record[2] != b' ' {
            continue;
        }
        let (index, worktree) = (record[0], record[1]);
        let Ok(path) = std::str::from_utf8(&record[3..]) else {
            continue;
        };
        // Rename and copy records are `XY <new>\0<old>\0`.
        let renamed = matches!(index, b'R' | b'C') || matches!(worktree, b'R' | b'C');
        let old = if renamed {
            let Some(split) = rest.iter().position(|byte| *byte == 0) else {
                break;
            };
            let old = std::str::from_utf8(&rest[..split]).ok();
            rest = &rest[split + 1..];
            old
        } else {
            None
        };
        if let Some(old) = old
            && let Some(entry) = entry_from(old, GitChange::Deleted, false)
        {
            entries.push(entry);
        }
        if let Some(entry) = entry_from(path, classify(index, worktree), path.ends_with('/')) {
            entries.push(entry);
        }
    }
    entries
}

fn entry_from(path: &str, change: GitChange, directory: bool) -> Option<GitEntry> {
    let path = path.trim_end_matches('/');
    if path.is_empty()
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(GitEntry {
        path: path.to_string(),
        change,
        directory,
    })
}

fn classify(index: u8, worktree: u8) -> GitChange {
    if index == b'?' && worktree == b'?' {
        return GitChange::Untracked;
    }
    if index == b'U'
        || worktree == b'U'
        || (index == b'A' && worktree == b'A')
        || (index == b'D' && worktree == b'D')
    {
        return GitChange::Conflict;
    }
    match if worktree == b' ' { index } else { worktree } {
        b'A' => GitChange::Added,
        b'D' => GitChange::Deleted,
        _ => GitChange::Modified,
    }
}

fn scope(entries: Vec<GitEntry>, prefix: &str) -> Vec<GitEntry> {
    entries
        .into_iter()
        .filter_map(|entry| {
            Some(GitEntry {
                path: relative_to_prefix(&entry.path, prefix)?,
                ..entry
            })
        })
        .collect()
}

fn relative_to_prefix(path: &str, prefix: &str) -> Option<String> {
    let relative = if prefix.is_empty() {
        path.to_string()
    } else {
        path.strip_prefix(prefix)?.strip_prefix('/')?.to_string()
    };
    if relative.is_empty()
        || relative
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return None;
    }
    Some(relative)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, change: GitChange, directory: bool) -> GitEntry {
        GitEntry {
            path: path.to_string(),
            change,
            directory,
        }
    }

    #[test]
    fn porcelain_parses_renames_edits_and_untracked_directories() {
        let bytes = b"RM cad/axle_renamed.py\0cad/axle.py\0 M cad/brake.py\0A  cad/staged.py\0D  cad/removed.py\0UU cad/both.py\0?? cad/nested/\0?? extra/whole/solo.py\0?? ../outside.py\0";
        assert_eq!(
            parse_porcelain(bytes),
            vec![
                entry("cad/axle.py", GitChange::Deleted, false),
                entry("cad/axle_renamed.py", GitChange::Modified, false),
                entry("cad/brake.py", GitChange::Modified, false),
                entry("cad/staged.py", GitChange::Added, false),
                entry("cad/removed.py", GitChange::Deleted, false),
                entry("cad/both.py", GitChange::Conflict, false),
                entry("cad/nested", GitChange::Untracked, true),
                entry("extra/whole/solo.py", GitChange::Untracked, false),
            ]
        );
    }

    #[test]
    fn project_prefix_hides_changes_outside_the_folder() {
        let entries =
            parse_porcelain(b" M apps/desktop/src/main.py\0?? apps/desktop/vendor/\0?? other.py\0");
        assert_eq!(
            scope(entries, "apps/desktop"),
            vec![
                entry("src/main.py", GitChange::Modified, false),
                entry("vendor", GitChange::Untracked, true),
            ]
        );
        let rooted = parse_porcelain(b"?? cad/new.py\0");
        assert_eq!(
            scope(rooted, ""),
            vec![entry("cad/new.py", GitChange::Untracked, false)]
        );
    }

    struct Remove(PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git_run(directory: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn project_status_reads_changes_relative_to_the_opened_folder() {
        let root = std::env::temp_dir().join(format!("cadpad-git-{}", uuid::Uuid::new_v4()));
        let _remove = Remove(root.clone());
        let project = root.join("apps/desktop");
        std::fs::create_dir_all(project.join("src")).unwrap();
        assert!(git_run(&root, &["init", "-q"]), "git init");
        std::fs::write(project.join("src/main.py"), "a\n").unwrap();
        assert!(git_run(&root, &["add", "."]));
        assert!(git_run(
            &root,
            &[
                "-c",
                "user.email=test@example.com",
                "-c",
                "user.name=Test",
                "commit",
                "-q",
                "-m",
                "init",
            ]
        ));
        std::fs::write(project.join("src/main.py"), "a\nb\n").unwrap();
        std::fs::write(project.join("src/new.py"), "c\n").unwrap();
        std::fs::create_dir_all(project.join("vendor")).unwrap();
        std::fs::write(project.join("vendor/lib.py"), "d\n").unwrap();
        std::fs::write(root.join("other.py"), "e\n").unwrap();

        let value = project_status(&project);
        assert_eq!(value["repo"], true);
        let entries = value["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry["path"].as_str().unwrap().to_string(),
                    entry["change"].as_str().unwrap().to_string(),
                    entry["directory"].as_bool().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(entries.contains(&("src/main.py".into(), "modified".into(), false)));
        assert!(entries.contains(&("src/new.py".into(), "untracked".into(), false)));
        assert!(entries.contains(&("vendor".into(), "untracked".into(), true)));
        assert!(entries.iter().all(|(path, _, _)| path != "other.py"));
    }

    #[test]
    fn project_status_ignores_a_folder_outside_git() {
        let root = std::env::temp_dir().join(format!("cadpad-plain-{}", uuid::Uuid::new_v4()));
        let _remove = Remove(root.clone());
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(project_status(&root), empty());
    }
}

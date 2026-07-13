use anyhow::{Context, Result};
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLineType {
    Add,
    Remove,
    Context,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: DiffLineType,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    pub fn total_additions(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineType::Add)
            .count()
    }

    pub fn total_deletions(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineType::Remove)
            .count()
    }
}

pub fn diff_files(old_path: &str, new_path: &str) -> Result<Vec<FileDiff>> {
    let old_content =
        std::fs::read_to_string(old_path).context(format!("Failed to read {}", old_path))?;
    let new_content =
        std::fs::read_to_string(new_path).context(format!("Failed to read {}", new_path))?;

    let diff = TextDiff::from_lines(&old_content, &new_content);
    let hunks = build_hunks(&diff);

    Ok(vec![FileDiff {
        path: new_path.to_string(),
        old_path: Some(old_path.to_string()),
        hunks,
    }])
}

fn build_hunks(diff: &TextDiff<'_, '_, '_, str>) -> Vec<Hunk> {
    diff.grouped_ops(3)
        .into_iter()
        .map(|group| {
            let mut lines = Vec::new();
            let mut old_start = 0;
            let mut new_start = 0;
            let mut old_lines = 0;
            let mut new_lines = 0;
            let mut first = true;

            for op in &group {
                for change in diff.iter_changes(op) {
                    let (kind, old_line, new_line) = match change.tag() {
                        ChangeTag::Delete => {
                            (DiffLineType::Remove, Some(change.old_index().unwrap() + 1), None)
                        }
                        ChangeTag::Insert => {
                            (DiffLineType::Add, None, Some(change.new_index().unwrap() + 1))
                        }
                        ChangeTag::Equal => (
                            DiffLineType::Context,
                            Some(change.old_index().unwrap() + 1),
                            Some(change.new_index().unwrap() + 1),
                        ),
                    };

                    if first {
                        old_start = change.old_index().unwrap_or(0) + 1;
                        new_start = change.new_index().unwrap_or(0) + 1;
                        first = false;
                    }

                    match kind {
                        DiffLineType::Remove => old_lines += 1,
                        DiffLineType::Add => new_lines += 1,
                        DiffLineType::Context => {
                            old_lines += 1;
                            new_lines += 1;
                        }
                    }

                    lines.push(DiffLine {
                        kind,
                        old_line,
                        new_line,
                        content: change.value().to_string(),
                    });
                }
            }

            let header = format!(
                "@@ -{},{} +{},{} @@",
                old_start, old_lines, new_start, new_lines
            );

            Hunk { header, lines }
        })
        .collect()
}

pub fn git_diff(_watch: bool, staged: bool) -> Result<Vec<FileDiff>> {
    let repo = git2::Repository::discover(".").context("Not in a git repository")?;

    let diff_output = get_git_diff_output(&repo, staged, false)?;
    let mut files = parse_unified_diff(&diff_output)?;

    if !staged {
        untracked_files(&repo, &mut files)?;
    }

    Ok(files)
}

fn get_git_diff_output(repo: &git2::Repository, staged: bool, show: bool) -> Result<String> {
    let mut diff_opts = git2::DiffOptions::new();
    diff_opts.include_untracked(false);
    diff_opts.ignore_submodules(true);
    diff_opts.context_lines(3);

    let diff = if show {
        repo.diff_tree_to_tree(None, None, Some(&mut diff_opts))
            .context("Failed to diff")?
    } else if staged {
        let head = repo.head().ok();
        let head_tree = head.as_ref().and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut diff_opts))
            .context("Failed to diff staged changes")?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut diff_opts))
            .context("Failed to diff working directory")?
    };

    let mut output = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let origin = line.origin();
        let content = String::from_utf8_lossy(line.content());
        match origin {
            'F' | 'H' => {
                output.push_str(&content);
            }
            _ => {
                output.push(origin);
                output.push_str(&content);
            }
        }
        true
    })
    .context("Failed to print diff")?;

    Ok(output)
}

fn untracked_files(repo: &git2::Repository, files: &mut Vec<FileDiff>) -> Result<()> {
    let statuses = repo
        .statuses(None)
        .context("Failed to get repository status")?;

    for entry in statuses.iter() {
        if entry.status().contains(git2::Status::WT_NEW) {
            if let Some(path) = entry.path() {
                if let Ok(content) = std::fs::read_to_string(repo.workdir().unwrap().join(path)) {
                    let diff = TextDiff::from_lines("", &content);
                    let hunks = build_hunks(&diff);

                    files.push(FileDiff {
                        path: path.to_string(),
                        old_path: None,
                        hunks,
                    });
                }
            }
        }
    }

    Ok(())
}

pub fn git_show(revision: Option<&str>) -> Result<Vec<FileDiff>> {
    let repo = git2::Repository::discover(".").context("Not in a git repository")?;

    let rev = revision.unwrap_or("HEAD");
    let obj = repo.revparse_single(rev).context("Invalid revision")?;
    let commit = obj.peel_to_commit().context("Not a commit")?;

    let parent_tree = commit.parent(0).ok().and_then(|p| p.tree().ok());
    let commit_tree = commit.tree().context("No tree")?;

    let mut diff_opts = git2::DiffOptions::new();
    diff_opts.context_lines(3);

    let diff = repo
        .diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), Some(&mut diff_opts))
        .context("Failed to diff")?;

    let mut output = String::new();
    diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let origin = line.origin();
        let content = String::from_utf8_lossy(line.content());
        match origin {
            'F' | 'H' => {
                output.push_str(&content);
            }
            _ => {
                output.push(origin);
                output.push_str(&content);
            }
        }
        true
    })
    .context("Failed to print diff")?;

    parse_unified_diff(&output)
}

pub fn parse_patch(file: Option<&str>) -> Result<Vec<FileDiff>> {
    let content = match file {
        None | Some("-") => {
            use std::io::Read;
            let mut input = String::new();
            std::io::stdin()
                .read_to_string(&mut input)
                .context("Failed to read stdin")?;
            input
        }
        Some(path) => {
            std::fs::read_to_string(path).context(format!("Failed to read {}", path))?
        }
    };

    parse_unified_diff(&content)
}

pub fn read_stdin_patch() -> Result<Vec<FileDiff>> {
    use std::io::Read;
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("Failed to read stdin")?;
    parse_unified_diff(&input)
}

fn parse_unified_diff(content: &str) -> Result<Vec<FileDiff>> {
    let mut files: Vec<FileDiff> = Vec::new();
    let mut current_file: Option<FileDiff> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in content.lines() {
        if line.starts_with("diff --git ") {
            if let Some(file) = current_file.take() {
                files.push(file);
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            let path = if parts.len() >= 4 {
                let p = parts[3];
                p.trim_start_matches("b/")
                    .trim_start_matches("w/")
                    .to_string()
            } else {
                "unknown".to_string()
            };
            current_file = Some(FileDiff {
                path,
                old_path: None,
                hunks: Vec::new(),
            });
        } else if line.starts_with("--- ") {
            if let Some(ref mut file) = current_file {
                let path = line[4..]
                    .trim()
                    .trim_start_matches("a/")
                    .trim_start_matches("i/");
                if file.old_path.is_none() && path != "/dev/null" {
                    file.old_path = Some(path.to_string());
                }
            }
        } else if line.starts_with("+++ ") {
            if let Some(ref mut file) = current_file {
                let path = line[4..]
                    .trim()
                    .trim_start_matches("b/")
                    .trim_start_matches("w/");
                if file.path == "unknown" && path != "/dev/null" {
                    file.path = path.to_string();
                }
            }
        } else if line.starts_with("index ") || line.starts_with("old mode ") || line.starts_with("new mode ") || line.starts_with("deleted file ") || line.starts_with("new file ") || line.starts_with("rename ") || line.starts_with("similarity ") {
            // skip metadata lines
            continue;
        } else if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current_file {
                    file.hunks.push(hunk);
                }
            }

            let header = line.to_string();
            current_hunk = Some(Hunk {
                header,
                lines: Vec::new(),
            });
        } else if let Some(ref mut hunk) = current_hunk {
            let (kind, old_line, new_line) = if line.starts_with('+') {
                (DiffLineType::Add, None, None)
            } else if line.starts_with('-') {
                (DiffLineType::Remove, None, None)
            } else if line.starts_with(' ') {
                (DiffLineType::Context, None, None)
            } else {
                continue;
            };

            hunk.lines.push(DiffLine {
                kind,
                old_line,
                new_line,
                content: line[1..].to_string(),
            });
        }
    }

    if let Some(hunk) = current_hunk {
        if let Some(ref mut file) = current_file {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current_file {
        files.push(file);
    }

    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_unified_diff() {
        let patch = r#"diff --git a/test.txt b/test.txt
--- a/test.txt
+++ b/test.txt
@@ -1,3 +1,4 @@
 hello
 world
+new line
 goodbye"#;

        let files = parse_unified_diff(patch).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "test.txt");
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].total_additions(), 1);
        assert_eq!(files[0].total_deletions(), 0);
        assert_eq!(files[0].hunks[0].lines.len(), 4);
    }

    #[test]
    fn test_parse_git_diff_with_index() {
        let patch = r#"diff --git i/test.txt w/test.txt
index ce01362..94954ab 100644
--- i/test.txt
+++ w/test.txt
@@ -1 +1,2 @@
 hello
+world"#;

        let files = parse_unified_diff(patch).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "test.txt");
        assert_eq!(files[0].old_path.as_deref(), Some("test.txt"));
    }

    #[test]
    fn test_parse_multiple_files() {
        let patch = r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,3 @@
 foo
+bar
 baz
diff --git a/b.txt b/b.txt
--- a/b.txt
+++ b/b.txt
@@ -1,2 +1,2 @@
-old
+new
 unchanged"#;

        let files = parse_unified_diff(patch).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[1].path, "b.txt");
    }

    #[test]
    fn test_empty_content() {
        let files = parse_unified_diff("").unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn test_git_diff_integration() {
        let dir = tempfile::tempdir().expect("Failed to create temp dir");

        // Init git repo
        let repo = git2::Repository::init(dir.path()).expect("Failed to init repo");

        // Create initial file
        let file_path = dir.path().join("test.txt");
        std::fs::write(&file_path, "hello\nworld\n").expect("Failed to write file");

        // Add and commit
        let mut index = repo.index().expect("Failed to get index");
        index
            .add_path(std::path::Path::new("test.txt"))
            .expect("Failed to add file");
        index.write().expect("Failed to write index");

        let tree_id = index.write_tree().expect("Failed to write tree");
        let sig = git2::Signature::now("test", "test@test.com").expect("Failed to create sig");
        let tree = repo.find_tree(tree_id).expect("Failed to find tree");
        repo.commit(
            Some("HEAD"),
            &sig,
            &sig,
            "initial",
            &tree,
            &[],
        )
        .expect("Failed to commit");

        // Modify the file
        std::fs::write(&file_path, "hello\nworld\nnew line\n").expect("Failed to write file");

        // Change to the temp dir and run git_diff
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).expect("Failed to change dir");
        let result = git_diff(false, false);
        std::env::set_current_dir(original_dir).expect("Failed to restore dir");

        let files = result.expect("Failed to git_diff");

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "test.txt");
        assert_eq!(files[0].hunks.len(), 1);
        assert_eq!(files[0].total_additions(), 1);
        assert_eq!(files[0].total_deletions(), 0);
    }
}

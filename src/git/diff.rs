use std::path::Path;

pub struct DiffLine {
    pub content: String,
    pub kind: DiffLineKind,
}

pub enum DiffLineKind {
    Header,
    Added,
    Removed,
    Context,
}

pub struct FileDiff {
    pub path: String,
    pub staged: bool,
    pub lines: Vec<DiffLine>,
}

/// Get diff for a single file.
/// staged=true → HEAD vs index (what would be committed)
/// staged=false → index vs workdir (unstaged changes)
pub fn get_file_diff(workdir: &Path, path: &str, staged: bool) -> Option<FileDiff> {
    let repo = git2::Repository::discover(workdir).ok()?;

    let mut opts = git2::DiffOptions::new();
    opts.pathspec(path);

    let diff = if staged {
        let head_tree = repo
            .head()
            .ok()
            .and_then(|h| h.peel_to_tree().ok());
        repo.diff_tree_to_index(head_tree.as_ref(), None, Some(&mut opts))
            .ok()?
    } else {
        repo.diff_index_to_workdir(None, Some(&mut opts)).ok()?
    };

    let mut lines: Vec<DiffLine> = Vec::new();

    let _ = diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
        let content = std::str::from_utf8(line.content())
            .unwrap_or("")
            .to_string();

        let kind = match line.origin() {
            '+' => DiffLineKind::Added,
            '-' => DiffLineKind::Removed,
            ' ' => DiffLineKind::Context,
            // Hunk headers, file headers, binary markers, etc.
            'H' | 'F' | 'B' | 'O' | 'E' | 'S' => DiffLineKind::Header,
            // '@' for hunk header line
            _ => DiffLineKind::Header,
        };

        lines.push(DiffLine { content, kind });
        true
    });

    Some(FileDiff {
        path: path.to_string(),
        staged,
        lines,
    })
}

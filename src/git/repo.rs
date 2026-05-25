use std::path::{Path, PathBuf};

/// Detect git workdir root by walking up from `path`.
/// Returns the workdir root (not .git) if found.
pub fn detect_repo(path: &Path) -> Option<PathBuf> {
    git2::Repository::discover(path)
        .ok()
        .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
}

pub struct StatusEntry {
    pub path: String,
    pub old_path: Option<String>, // for renames
    pub status: FileStatus,
}

pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    TypeChange,
}

pub struct GitStatus {
    pub staged: Vec<StatusEntry>,
    pub unstaged: Vec<StatusEntry>,
    pub untracked: Vec<StatusEntry>,
    pub head_branch: Option<String>,
    pub is_detached: bool,
}

pub struct BranchInfo {
    pub name: String,
    pub is_head: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
}

pub struct StashEntry {
    pub index: usize,
    pub message: String,
}

/// Load current git status. Returns None if not a git repo.
pub fn load_status(workdir: &Path) -> Option<GitStatus> {
    let repo = git2::Repository::discover(workdir).ok()?;

    let head_branch = repo.head().ok().and_then(|h| h.shorthand().map(String::from));
    let is_detached = repo.head_detached().unwrap_or(false);

    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true);
    opts.recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts)).ok()?;

    let mut staged = Vec::new();
    let mut unstaged = Vec::new();
    let mut untracked = Vec::new();

    for entry in statuses.iter() {
        let path = match entry.path() {
            Some(p) => p.to_string(),
            None => continue,
        };

        let st = entry.status();

        // Handle untracked separately
        if st.contains(git2::Status::WT_NEW) && !st.intersects(
            git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_RENAMED
                | git2::Status::INDEX_TYPECHANGE,
        ) {
            untracked.push(StatusEntry {
                path: path.clone(),
                old_path: None,
                status: FileStatus::Untracked,
            });
            continue;
        }

        // Staged changes (INDEX_*)
        if st.intersects(
            git2::Status::INDEX_NEW
                | git2::Status::INDEX_MODIFIED
                | git2::Status::INDEX_DELETED
                | git2::Status::INDEX_RENAMED
                | git2::Status::INDEX_TYPECHANGE,
        ) {
            let file_status = if st.contains(git2::Status::INDEX_NEW) {
                FileStatus::Added
            } else if st.contains(git2::Status::INDEX_DELETED) {
                FileStatus::Deleted
            } else if st.contains(git2::Status::INDEX_RENAMED) {
                FileStatus::Renamed
            } else if st.contains(git2::Status::INDEX_TYPECHANGE) {
                FileStatus::TypeChange
            } else {
                FileStatus::Modified
            };

            let old_path = if st.contains(git2::Status::INDEX_RENAMED) {
                entry.head_to_index().and_then(|d| {
                    d.old_file().path().and_then(|p| p.to_str()).map(String::from)
                })
            } else {
                None
            };

            staged.push(StatusEntry {
                path: path.clone(),
                old_path,
                status: file_status,
            });
        }

        // Unstaged changes (WT_*)
        if st.intersects(
            git2::Status::WT_MODIFIED
                | git2::Status::WT_DELETED
                | git2::Status::WT_RENAMED
                | git2::Status::WT_TYPECHANGE,
        ) {
            let file_status = if st.contains(git2::Status::WT_DELETED) {
                FileStatus::Deleted
            } else if st.contains(git2::Status::WT_RENAMED) {
                FileStatus::Renamed
            } else if st.contains(git2::Status::WT_TYPECHANGE) {
                FileStatus::TypeChange
            } else {
                FileStatus::Modified
            };

            unstaged.push(StatusEntry {
                path: path.clone(),
                old_path: None,
                status: file_status,
            });
        }
    }

    Some(GitStatus {
        staged,
        unstaged,
        untracked,
        head_branch,
        is_detached,
    })
}

/// Load all branches (local + remote).
pub fn load_branches(workdir: &Path) -> Vec<BranchInfo> {
    let repo = match git2::Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut branches = Vec::new();

    let iter = match repo.branches(None) {
        Ok(i) => i,
        Err(_) => return Vec::new(),
    };

    for item in iter {
        let (branch, branch_type) = match item {
            Ok(b) => b,
            Err(_) => continue,
        };

        let name = match branch.name() {
            Ok(Some(n)) => n.to_string(),
            _ => continue,
        };

        let is_head = branch.is_head();
        let is_remote = branch_type == git2::BranchType::Remote;
        let upstream = branch
            .upstream()
            .ok()
            .and_then(|u| u.name().ok().flatten().map(String::from));

        branches.push(BranchInfo {
            name,
            is_head,
            is_remote,
            upstream,
        });
    }

    branches
}

/// Load git config entries. If `global` is true, reads `--global`; otherwise reads `--local`.
/// Returns (key, value) pairs.
pub fn load_git_config(workdir: &Path, global: bool) -> Vec<(String, String)> {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(workdir);
    if global {
        cmd.args(["config", "--list", "--global"]);
    } else {
        cmd.args(["config", "--list", "--local"]);
    }
    match cmd.output() {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(2, '=');
                    let key = parts.next()?.trim().to_string();
                    let value = parts.next().unwrap_or("").to_string();
                    if key.is_empty() { return None; }
                    Some((key, value))
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Load stash list.
pub fn load_stashes(workdir: &Path) -> Vec<StashEntry> {
    let mut repo = match git2::Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut stashes = Vec::new();

    let _ = repo.stash_foreach(|index, msg, _oid| {
        stashes.push(StashEntry {
            index,
            message: msg.to_string(),
        });
        true
    });

    stashes
}

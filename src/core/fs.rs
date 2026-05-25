use std::path::{Path, PathBuf};
use std::time::SystemTime;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    Directory,
    File,
    Symlink,
}

impl std::fmt::Display for FileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileKind::Directory => write!(f, "Folder"),
            FileKind::File => write!(f, "File"),
            FileKind::Symlink => write!(f, "Symlink"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: PathBuf,
    pub kind: FileKind,
    pub size: Option<u64>,
    pub modified: Option<SystemTime>,
    #[serde(skip)]
    pub tags: Vec<crate::core::tags::Tag>,
}

impl FileEntry {
    pub fn size_display(&self) -> String {
        match self.size {
            None | Some(0) => "—".to_string(),
            Some(b) if b < 1_024 => format!("{} B", b),
            Some(b) if b < 1_024 * 1_024 => format!("{:.1} KB", b as f64 / 1_024.0),
            Some(b) if b < 1_024 * 1_024 * 1_024 => {
                format!("{:.1} MB", b as f64 / (1_024.0 * 1_024.0))
            }
            Some(b) => format!("{:.1} GB", b as f64 / (1_024.0 * 1_024.0 * 1_024.0)),
        }
    }

    pub fn modified_display(&self) -> String {
        use std::time::UNIX_EPOCH;
        match self.modified {
            None => "—".to_string(),
            Some(t) => {
                let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
                // Format as YYYY-MM-DD HH:MM
                let dt = secs as i64;
                let s = dt % 60;
                let m = (dt / 60) % 60;
                let h = (dt / 3600) % 24;
                let days = dt / 86400;
                let _ = s;
                // Simple approximation for display
                let year = 1970 + days / 365;
                let day_of_year = days % 365;
                let month = day_of_year / 30 + 1;
                let day = day_of_year % 30 + 1;
                format!("{:04}-{:02}-{:02} {:02}:{:02}", year, month, day, h, m)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Size,
    Kind,
    Modified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Ascending,
    Descending,
}

pub fn read_dir(path: &Path, show_hidden: bool) -> Vec<FileEntry> {
    let mut entries = Vec::new();

    let Ok(dir) = std::fs::read_dir(path) else {
        return entries;
    };

    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let meta = entry.metadata().ok();
        let kind = if let Some(ref m) = meta {
            if m.is_dir() {
                FileKind::Directory
            } else if m.is_symlink() {
                FileKind::Symlink
            } else {
                FileKind::File
            }
        } else {
            FileKind::File
        };

        let size = meta.as_ref().and_then(|m| {
            if m.is_file() {
                Some(m.len())
            } else {
                None
            }
        });

        let modified = meta.as_ref().and_then(|m| m.modified().ok());

        entries.push(FileEntry {
            name,
            path: entry.path(),
            kind,
            size,
            modified,
            tags: crate::core::tags::read_tags(&entry.path()),
        });
    }

    entries
}

pub fn sort_entries(entries: &mut Vec<FileEntry>, col: SortColumn, order: SortOrder) {
    entries.sort_by(|a, b| {
        // Directories always first
        let dir_cmp = matches!(b.kind, FileKind::Directory)
            .cmp(&matches!(a.kind, FileKind::Directory));
        if dir_cmp != std::cmp::Ordering::Equal {
            return dir_cmp;
        }

        let ord = match col {
            SortColumn::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortColumn::Kind => a.kind.to_string().cmp(&b.kind.to_string()),
            SortColumn::Size => a.size.unwrap_or(0).cmp(&b.size.unwrap_or(0)),
            SortColumn::Modified => a.modified.cmp(&b.modified),
        };

        if order == SortOrder::Descending {
            ord.reverse()
        } else {
            ord
        }
    });
}

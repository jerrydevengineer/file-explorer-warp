use std::path::PathBuf;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalTag {
    pub name: String,
    pub color: u8, // 0 = none/gray, 1-7 = standard colors
}

impl GlobalTag {
    pub fn rgb(&self) -> (u8, u8, u8) {
        crate::core::tags::TagColor::from_number(self.color).rgb()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GlobalTags {
    pub items: Vec<GlobalTag>,
}

/// Standard Finder colors seeded on first launch.
const SEED_TAGS: &[(&str, u8)] = &[
    ("Red", 6),
    ("Orange", 7),
    ("Yellow", 5),
    ("Green", 2),
    ("Blue", 4),
    ("Purple", 3),
    ("Gray", 1),
];

impl GlobalTags {
    pub fn load() -> Self {
        let path = config_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            // First launch — seed with the standard Finder colors
            let mut tags = Self::default();
            for &(name, color) in SEED_TAGS {
                tags.items.push(GlobalTag { name: name.to_string(), color });
            }
            tags.save();
            tags
        }
    }

    pub fn save(&self) {
        let path = config_path();
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }

    /// Add a tag. Returns false if a tag with the same name already exists.
    pub fn add(&mut self, name: String, color: u8) -> bool {
        if self.items.iter().any(|t| t.name == name) {
            return false;
        }
        self.items.push(GlobalTag { name, color });
        self.save();
        true
    }

    pub fn remove(&mut self, idx: usize) {
        if idx < self.items.len() {
            self.items.remove(idx);
            self.save();
        }
    }

    pub fn rename(&mut self, idx: usize, name: String) {
        if idx < self.items.len() {
            self.items[idx].name = name;
            self.save();
        }
    }

    pub fn set_color(&mut self, idx: usize, color: u8) {
        if idx < self.items.len() {
            self.items[idx].color = color;
            self.save();
        }
    }
}

/// Search for files/folders tagged with `name` using macOS Spotlight (mdfind).
/// Returns matching paths sorted by filename. Spotlight may lag a few seconds
/// after a new tag is applied before it appears in results.
pub fn search_by_tag(name: &str) -> Vec<PathBuf> {
    let query = format!("kMDItemUserTags == '{}'", name.replace('\'', "\\'"));
    match std::process::Command::new("mdfind").arg(&query).output() {
        Ok(o) if o.status.success() => {
            let mut paths: Vec<PathBuf> = String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect();
            paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
            paths
        }
        _ => Vec::new(),
    }
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = PathBuf::from(home).join(".config").join("file-explorer");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("tags.json")
}

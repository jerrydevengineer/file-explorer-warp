use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::core::display_text;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
}

impl Bookmarks {
    fn config_path() -> PathBuf {
        let base = dirs_next();
        base.join("bookmarks.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if let Ok(data) = std::fs::read_to_string(&path) {
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, data);
        }
    }

    pub fn add(&mut self, path: PathBuf) {
        if self.items.iter().any(|b| b.path == path) {
            return;
        }
        let name = if path.file_name().is_some() {
            display_text::file_name(&path)
        } else {
            display_text::path(&path)
        };
        self.items.push(Bookmark { name, path });
        self.save();
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
            self.save();
        }
    }
}

fn dirs_next() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config").join("file-explorer")
}

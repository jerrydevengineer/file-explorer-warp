use nucleo::pattern::{CaseMatching, Normalization};
use nucleo::{Config, Nucleo};
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::display_text;

pub struct SearchEngine {
    nucleo: Nucleo<PathBuf>,
    pub root: PathBuf,
    last_query: String,
}

impl SearchEngine {
    /// Spawn a background walk of `root` and create a live fuzzy matcher.
    /// `notify` is called (from nucleo's internal thread) whenever new results
    /// are ready; pass `Arc::new(move || ctx.request_repaint())` from the UI.
    pub fn new(root: PathBuf, notify: Arc<dyn Fn() + Sync + Send>) -> Self {
        let nucleo = Nucleo::<PathBuf>::new(Config::DEFAULT, notify, None, 1);
        let injector = nucleo.injector();
        let root_clone = root.clone();

        std::thread::spawn(move || {
            for entry in walkdir::WalkDir::new(&root_clone)
                .follow_links(false)
                .max_depth(12)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                // Skip the root itself
                if entry.depth() == 0 {
                    continue;
                }

                let path = entry.path().to_path_buf();
                // Match column: path relative to root (e.g. "src/core/fs.rs")
                let display = path
                    .strip_prefix(&root_clone)
                    .map(display_text::path)
                    .unwrap_or_else(|_| display_text::path(&path));

                let _ = injector.push(path, move |_, cols| {
                    cols[0] = display.into();
                });
            }
        });

        Self {
            nucleo,
            root,
            last_query: String::new(),
        }
    }

    /// Update the fuzzy pattern when the user's query changes.
    pub fn set_query(&mut self, query: &str) {
        let normalized_query = display_text::normalize(query);
        if self.last_query == normalized_query {
            return;
        }
        self.last_query = normalized_query.clone();
        self.nucleo.pattern.reparse(
            0,
            &normalized_query,
            CaseMatching::Smart,
            Normalization::Never,
            false,
        );
    }

    /// Drive nucleo for up to 10 ms. Call once per frame while the overlay is open.
    pub fn tick(&mut self) {
        self.nucleo.tick(10);
    }

    /// Return at most `limit` matched paths, best score first.
    pub fn results(&self, limit: u32) -> Vec<PathBuf> {
        let snapshot = self.nucleo.snapshot();
        let count = snapshot.matched_item_count().min(limit);
        (0..count)
            .filter_map(|i| snapshot.get_matched_item(i))
            .map(|item| item.data.clone())
            .collect()
    }
}

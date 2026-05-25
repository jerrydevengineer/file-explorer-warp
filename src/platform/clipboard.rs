use std::path::Path;

pub fn copy_path(path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(path_str);
    }
}

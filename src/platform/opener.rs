use std::path::Path;
use std::process::Command;
use crate::core::config::TerminalApp;

/// Open a file with the default application.
pub fn open_file(path: &Path) {
    let _ = Command::new("open").arg(path).spawn();
}

/// Reveal a path in Finder.
pub fn reveal_in_finder(path: &Path) {
    let _ = Command::new("open").arg("-R").arg(path).spawn();
}

/// Move a file or folder to the macOS Trash.
/// Returns an error string on failure.
pub fn trash_path(path: &Path) -> Result<(), String> {
    let escaped = path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = format!(
        r#"tell application "Finder" to delete POSIX file "{}""#,
        escaped
    );
    let status = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("Finder could not move the item to Trash".to_string())
    }
}

/// Open the native macOS Get Info window for a file or folder.
pub fn get_info(path: &Path) {
    let script = format!(
        r#"tell application "Finder" to open information window of (POSIX file "{}" as alias)"#,
        path.to_string_lossy()
    );
    let _ = Command::new("osascript").arg("-e").arg(script).spawn();
}

/// Open a directory in the user's preferred terminal.
pub fn open_in_terminal(path: &Path, terminal: TerminalApp) {
    let dir = if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    };

    match terminal {
        TerminalApp::Terminal => open_terminal_app(&dir),
        TerminalApp::ITerm2 => { open_iterm2(&dir); }
        TerminalApp::Warp => { open_warp(&dir); }
        TerminalApp::Ghostty => { open_ghostty(&dir); }
        TerminalApp::Auto => {
            if open_iterm2(&dir) { return; }
            if open_warp(&dir) { return; }
            if open_ghostty(&dir) { return; }
            open_terminal_app(&dir);
        }
    }
}

fn open_iterm2(dir: &std::path::Path) -> bool {
    Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"tell application "iTerm2"
                create window with default profile
                tell current session of current window
                    write text "cd '{}'"
                end tell
            end tell"#,
            dir.to_string_lossy()
        ))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn open_warp(dir: &std::path::Path) -> bool {
    Command::new("open")
        .arg("-a")
        .arg("Warp")
        .arg(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn open_ghostty(dir: &std::path::Path) -> bool {
    Command::new("open")
        .arg("-a")
        .arg("Ghostty")
        .arg(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn open_terminal_app(dir: &std::path::Path) {
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(format!(
            r#"tell application "Terminal"
                do script "cd '{}'"
                activate
            end tell"#,
            dir.to_string_lossy()
        ))
        .spawn();
}

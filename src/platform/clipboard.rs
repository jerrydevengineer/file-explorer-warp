use std::path::{Path, PathBuf};

pub fn copy_path(path: &Path) {
    let path_str = path.to_string_lossy().to_string();
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(path_str);
    }
}

#[derive(Debug, Clone)]
pub struct FileClipboard {
    pub paths: Vec<PathBuf>,
    pub change_count: i64,
}

#[cfg(target_os = "macos")]
pub fn write_files(paths: &[PathBuf]) -> Result<i64, String> {
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{NSPasteboard, NSPasteboardWriting};
    use objc2_foundation::{NSArray, NSString, NSURL};

    if paths.is_empty() {
        return Err("No files selected".to_string());
    }

    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
    let mut objects = Vec::with_capacity(paths.len() + 1);
    for path in paths {
        let path = NSString::from_str(&path.to_string_lossy());
        let url = unsafe { NSURL::fileURLWithPath(&path) };
        objects.push(ProtocolObject::<dyn NSPasteboardWriting>::from_retained(url));
    }
    // egui-winit emits Event::Paste only when the pasteboard also exposes a
    // text object. NSURL objects remain authoritative for file operations.
    let text = paths
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    let text = NSString::from_str(&text);
    objects.push(ProtocolObject::<dyn NSPasteboardWriting>::from_retained(text));
    let objects = NSArray::from_vec(objects);
    unsafe {
        pasteboard.clearContents();
        if !pasteboard.writeObjects(&objects) {
            return Err("macOS rejected the file clipboard contents".to_string());
        }
        Ok(pasteboard.changeCount() as i64)
    }
}

#[cfg(target_os = "macos")]
pub fn read_files() -> Result<FileClipboard, String> {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::ClassType;
    use objc2_app_kit::NSPasteboard;
    use objc2_foundation::{NSArray, NSURL};

    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
    let url_class = {
        let class: *const AnyClass = NSURL::class();
        let object = class as *mut AnyObject;
        unsafe { Retained::retain(object) }
            .ok_or_else(|| "Could not access the NSURL pasteboard class".to_string())?
    };
    let classes = NSArray::from_vec(vec![url_class]);
    let objects = unsafe { pasteboard.readObjectsForClasses_options(&classes, None) }
        .ok_or_else(|| "Clipboard does not contain files".to_string())?;

    let mut paths = Vec::with_capacity(objects.len());
    for object in objects.iter() {
        let url = unsafe { &*(object as *const AnyObject as *const NSURL) };
        if !unsafe { url.isFileURL() } {
            continue;
        }
        if let Some(path) = unsafe { url.path() } {
            let path = PathBuf::from(path.to_string());
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    if paths.is_empty() {
        return Err("Clipboard does not contain files".to_string());
    }
    Ok(FileClipboard {
        paths,
        change_count: unsafe { pasteboard.changeCount() as i64 },
    })
}

#[cfg(not(target_os = "macos"))]
pub fn write_files(paths: &[PathBuf]) -> Result<i64, String> {
    let text = paths
        .iter()
        .map(|path| path.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
    clipboard.set_text(text).map_err(|error| error.to_string())?;
    Ok(0)
}

#[cfg(not(target_os = "macos"))]
pub fn read_files() -> Result<FileClipboard, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
    let text = clipboard.get_text().map_err(|error| error.to_string())?;
    let paths: Vec<PathBuf> = text
        .lines()
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .collect();
    if paths.is_empty() {
        return Err("Clipboard does not contain files".to_string());
    }
    Ok(FileClipboard {
        paths,
        change_count: 0,
    })
}

pub fn has_files() -> bool {
    read_files().map_or(false, |clipboard| !clipboard.paths.is_empty())
}

#[cfg(target_os = "macos")]
pub fn change_count() -> i64 {
    use objc2_app_kit::NSPasteboard;
    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
    unsafe { pasteboard.changeCount() as i64 }
}

#[cfg(not(target_os = "macos"))]
pub fn change_count() -> i64 { 0 }

#[cfg(target_os = "macos")]
pub fn clear_files() {
    use objc2_app_kit::NSPasteboard;
    let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
    unsafe { pasteboard.clearContents(); }
}

#[cfg(not(target_os = "macos"))]
pub fn clear_files() {
    if let Ok(mut clipboard) = arboard::Clipboard::new() {
        let _ = clipboard.set_text(String::new());
    }
}

use std::ffi::OsStr;
use std::path::Path;

use unicode_normalization::UnicodeNormalization;

/// Normalize text for presentation without changing the value used for
/// filesystem identity or operations.
pub fn normalize(text: &str) -> String {
    text.nfc().collect()
}

/// Convert filesystem text to a normalized, lossy display string.
pub fn os_str(value: &OsStr) -> String {
    normalize(value.to_string_lossy().as_ref())
}

/// Return a normalized filename for display only.
pub fn file_name(path: &Path) -> String {
    path.file_name().map(os_str).unwrap_or_default()
}

/// Return a normalized path for display only.
pub fn path(path: &Path) -> String {
    normalize(path.to_string_lossy().as_ref())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{normalize, path};

    #[test]
    fn nfc_korean_is_unchanged() {
        let text = "한글 파일.txt";
        assert_eq!(normalize(text), text);
    }

    #[test]
    fn nfd_korean_normalizes_to_the_same_label() {
        let nfd = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}";
        assert_eq!(normalize(nfd), "한글");
        assert_eq!(normalize(nfd), normalize("한글"));
    }

    #[test]
    fn mixed_korean_and_latin_text_is_preserved() {
        let text = "release-한국어-v2.txt";
        assert_eq!(normalize(text), text);
    }

    #[test]
    fn combining_accents_normalize_without_panicking() {
        assert_eq!(normalize("cafe\u{301}.txt"), "café.txt");
    }

    #[test]
    fn deriving_display_text_does_not_change_the_path() {
        let raw = PathBuf::from("/tmp/\u{1112}\u{1161}\u{11ab}.txt");
        let original = raw.clone();

        assert_eq!(path(&raw), "/tmp/한.txt");
        assert_eq!(raw, original);
    }
}

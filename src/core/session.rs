use serde::{Deserialize, Serialize};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SESSION_VERSION: u32 = 1;
const MAX_SESSION_BYTES: u64 = 1024 * 1024;
const SESSION_FILE_NAME: &str = "session-terminal.json";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    pub version: u32,
    pub left: PaneSession,
    #[serde(default)]
    pub right: Option<PaneSession>,
    #[serde(default)]
    pub focus: SavedPaneSide,
    #[serde(default = "default_split_ratio")]
    pub split_ratio: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSession {
    pub tabs: Vec<TabSession>,
    #[serde(default)]
    pub active_tab: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabSession {
    pub path: PathBuf,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedPaneSide {
    #[default]
    Left,
    Right,
}

#[derive(Debug)]
pub enum SessionError {
    Io(std::io::Error),
    Json(serde_json::Error),
    TooLarge(u64),
    UnsupportedVersion(u32),
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Json(error) => write!(formatter, "{error}"),
            Self::TooLarge(bytes) => {
                write!(formatter, "session file is too large ({bytes} bytes)")
            }
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported session version {version}")
            }
        }
    }
}

impl std::error::Error for SessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::TooLarge(_) | Self::UnsupportedVersion(_) => None,
        }
    }
}

impl From<std::io::Error> for SessionError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl SessionState {
    pub fn load() -> Result<Option<Self>, SessionError> {
        load_from(&session_path())
    }

    pub fn save(&self) -> Result<(), SessionError> {
        save_to(self, &session_path())
    }
}

pub fn session_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("file-explorer")
        .join(SESSION_FILE_NAME)
}

fn default_split_ratio() -> f32 {
    0.5
}

fn load_from(path: &Path) -> Result<Option<SessionState>, SessionError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.len() > MAX_SESSION_BYTES {
        return Err(SessionError::TooLarge(metadata.len()));
    }

    let data = std::fs::read_to_string(path)?;
    let session: SessionState = serde_json::from_str(&data)?;
    if session.version != SESSION_VERSION {
        return Err(SessionError::UnsupportedVersion(session.version));
    }
    Ok(Some(session))
}

fn save_to(session: &SessionState, path: &Path) -> Result<(), SessionError> {
    let data = serde_json::to_vec_pretty(session)?;
    if data.len() as u64 > MAX_SESSION_BYTES {
        return Err(SessionError::TooLarge(data.len() as u64));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = temporary_path(path);
    let write_result = (|| -> Result<(), SessionError> {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&data)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut file_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "session".into());
    file_name.push(format!(".tmp-{}", std::process::id()));
    path.with_file_name(file_name)
}

#[cfg(test)]
mod tests {
    use super::{
        load_from, save_to, session_path, PaneSession, SavedPaneSide, SessionError, SessionState,
        TabSession, SESSION_FILE_NAME, SESSION_VERSION,
    };
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "file-explorer-session-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            );
            let path = std::env::temp_dir().join(unique);
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn example_session() -> SessionState {
        SessionState {
            version: SESSION_VERSION,
            left: PaneSession {
                tabs: vec![
                    TabSession {
                        path: PathBuf::from("/Users/example/프로젝트"),
                    },
                    TabSession {
                        path: PathBuf::from("/Users/example/a long folder"),
                    },
                ],
                active_tab: 1,
            },
            right: Some(PaneSession {
                tabs: vec![TabSession {
                    path: PathBuf::from("/Users/example/Pictures"),
                }],
                active_tab: 0,
            }),
            focus: SavedPaneSide::Right,
            split_ratio: 0.42,
        }
    }

    #[test]
    fn round_trip_preserves_paths_and_layout() {
        let directory = TestDir::new("round-trip");
        let path = directory.0.join("session.json");
        let expected = example_session();

        save_to(&expected, &path).unwrap();

        assert_eq!(load_from(&path).unwrap(), Some(expected));
    }

    #[test]
    fn missing_file_has_no_session() {
        let directory = TestDir::new("missing");
        assert_eq!(load_from(&directory.0.join("missing.json")).unwrap(), None);
    }

    #[test]
    fn corrupt_json_is_reported() {
        let directory = TestDir::new("corrupt");
        let path = directory.0.join("session.json");
        std::fs::write(&path, "not-json").unwrap();

        assert!(matches!(load_from(&path), Err(SessionError::Json(_))));
    }

    #[test]
    fn unsupported_version_is_reported() {
        let directory = TestDir::new("version");
        let path = directory.0.join("session.json");
        let mut session = example_session();
        session.version += 1;
        std::fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();

        assert!(matches!(
            load_from(&path),
            Err(SessionError::UnsupportedVersion(_))
        ));
    }

    #[test]
    fn save_atomically_replaces_existing_session() {
        let directory = TestDir::new("replace");
        let path = directory.0.join("session.json");
        std::fs::write(&path, "old contents").unwrap();
        let expected = example_session();

        save_to(&expected, &path).unwrap();

        assert_eq!(load_from(&path).unwrap(), Some(expected));
        assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    }

    #[test]
    fn older_json_defaults_focus_and_split_ratio() {
        let directory = TestDir::new("defaults");
        let path = directory.0.join("session.json");
        std::fs::write(
            &path,
            r#"{"version":1,"left":{"tabs":[{"path":"/tmp"}],"active_tab":0}}"#,
        )
        .unwrap();

        let session = load_from(&path).unwrap().unwrap();
        assert_eq!(session.focus, SavedPaneSide::Left);
        assert_eq!(session.split_ratio, 0.5);
    }

    #[test]
    fn terminal_variant_uses_its_own_filename() {
        assert_eq!(
            session_path().file_name().and_then(|name| name.to_str()),
            Some(SESSION_FILE_NAME),
        );
    }
}

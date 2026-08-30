use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use notify::event::{AccessKind, AccessMode, ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

const QUIET_PERIOD: Duration = Duration::from_millis(160);
const MAX_BATCH_LATENCY: Duration = Duration::from_millis(500);
const WATCH_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Default)]
struct RefreshDebouncer {
    dirty_dirs: HashSet<PathBuf>,
    first_dirty_at: Option<Instant>,
    last_event_at: Option<Instant>,
}

impl RefreshDebouncer {
    fn mark<I>(&mut self, dirs: I, now: Instant)
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut added = false;
        for dir in dirs {
            added |= self.dirty_dirs.insert(dir);
        }
        if added || !self.dirty_dirs.is_empty() {
            self.first_dirty_at.get_or_insert(now);
            self.last_event_at = Some(now);
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        let quiet = self.last_event_at?.checked_add(QUIET_PERIOD)?;
        let maximum = self.first_dirty_at?.checked_add(MAX_BATCH_LATENCY)?;
        Some(quiet.min(maximum))
    }

    fn take_due(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        if self.dirty_dirs.is_empty() || self.next_deadline().is_none_or(|due| now < due) {
            return None;
        }
        let mut dirs: Vec<_> = self.dirty_dirs.drain().collect();
        dirs.sort();
        self.first_dirty_at = None;
        self.last_event_at = None;
        Some(dirs)
    }
}

#[derive(Default)]
pub struct WatchUpdate {
    pub directory_renames: Vec<(PathBuf, PathBuf)>,
    pub errors: Vec<String>,
}

pub struct DirectoryWatcher {
    watcher: RecommendedWatcher,
    receiver: mpsc::Receiver<notify::Result<Event>>,
    watched_paths: HashSet<PathBuf>,
    failed_paths: HashMap<PathBuf, Instant>,
    displayed_identities: HashMap<PathBuf, DirectoryIdentity>,
    debouncer: RefreshDebouncer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    device: u64,
    inode: u64,
}

impl DirectoryWatcher {
    pub fn new(repaint: Arc<dyn Fn() + Send + Sync>) -> notify::Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |result| {
            if sender.send(result).is_ok() {
                repaint();
            }
        })?;
        Ok(Self {
            watcher,
            receiver,
            watched_paths: HashSet::new(),
            failed_paths: HashMap::new(),
            displayed_identities: HashMap::new(),
            debouncer: RefreshDebouncer::default(),
        })
    }

    /// Reconcile OS watches with every open tab plus each tab directory's parent.
    /// Parent watches make deletion/rename of the displayed directory observable.
    pub fn sync_paths(&mut self, displayed_dirs: &[PathBuf], now: Instant) -> Vec<String> {
        let desired = desired_watch_paths(displayed_dirs);
        self.failed_paths.retain(|path, _| desired.contains(path));
        self.displayed_identities
            .retain(|path, _| displayed_dirs.contains(path));
        for directory in displayed_dirs {
            if let Some(identity) = directory_identity(directory) {
                self.displayed_identities
                    .insert(directory.clone(), identity);
            }
        }

        let removed: Vec<_> = self.watched_paths.difference(&desired).cloned().collect();
        for path in removed {
            if let Err(error) = self.watcher.unwatch(&path) {
                eprintln!("[watch] could not unwatch {}: {error}", path.display());
            }
            self.watched_paths.remove(&path);
        }

        let additions: Vec<_> = desired
            .difference(&self.watched_paths)
            .filter(|path| {
                self.failed_paths
                    .get(*path)
                    .is_none_or(|failed_at| now.duration_since(*failed_at) >= WATCH_RETRY_DELAY)
            })
            .cloned()
            .collect();
        let mut errors = Vec::new();
        for path in additions {
            match self.watcher.watch(&path, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    self.failed_paths.remove(&path);
                    self.watched_paths.insert(path);
                }
                Err(error) => {
                    self.failed_paths.insert(path.clone(), now);
                    errors.push(format!("Could not watch {}: {error}", path.display()));
                }
            }
        }
        errors
    }

    pub fn drain_events(&mut self, displayed_dirs: &[PathBuf], now: Instant) -> WatchUpdate {
        let mut update = WatchUpdate::default();
        while let Ok(result) = self.receiver.try_recv() {
            match result {
                Ok(event) if event_is_relevant(&event) => {
                    let affected = if event.need_rescan() {
                        displayed_dirs.to_vec()
                    } else {
                        affected_displayed_dirs(&event, displayed_dirs)
                    };
                    self.debouncer.mark(affected, now);
                    for rename in directory_rename_pairs(&event, displayed_dirs) {
                        if !update.directory_renames.contains(&rename) {
                            update.directory_renames.push(rename);
                        }
                    }
                    for directory in displayed_dirs {
                        let event_touches_parent = event
                            .paths
                            .iter()
                            .any(|path| path == directory || path.parent() == directory.parent());
                        if directory.is_dir() || !event_touches_parent {
                            continue;
                        }
                        let Some(identity) = self.displayed_identities.get(directory) else {
                            continue;
                        };
                        if let Some(new_path) = find_renamed_sibling(directory, *identity) {
                            let rename = (directory.clone(), new_path);
                            if !update.directory_renames.contains(&rename) {
                                update.directory_renames.push(rename);
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(error) => update.errors.push(error.to_string()),
            }
        }
        update
    }

    pub fn take_due_refresh(&mut self, now: Instant) -> Option<Vec<PathBuf>> {
        self.debouncer.take_due(now)
    }

    pub fn time_until_refresh(&self, now: Instant) -> Option<Duration> {
        self.debouncer
            .next_deadline()
            .map(|deadline| deadline.saturating_duration_since(now))
    }
}

fn desired_watch_paths(displayed_dirs: &[PathBuf]) -> HashSet<PathBuf> {
    let mut desired = HashSet::new();
    for directory in displayed_dirs {
        if directory.is_dir() {
            desired.insert(directory.clone());
        }
        if let Some(parent) = directory.parent().filter(|parent| parent.is_dir()) {
            desired.insert(parent.to_path_buf());
        }
    }
    desired
}

fn event_is_relevant(event: &Event) -> bool {
    match event.kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        _ => true,
    }
}

fn affected_displayed_dirs(event: &Event, displayed_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut affected = HashSet::new();
    for event_path in &event.paths {
        for directory in displayed_dirs {
            if event_path == directory || event_path.parent() == Some(directory.as_path()) {
                affected.insert(directory.clone());
            }
        }
    }
    affected.into_iter().collect()
}

fn directory_rename_pairs(event: &Event, displayed_dirs: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let is_pair = matches!(
        event.kind,
        EventKind::Modify(ModifyKind::Name(RenameMode::Both | RenameMode::Any))
    );
    if !is_pair || event.paths.len() != 2 {
        return Vec::new();
    }
    let old_path = &event.paths[0];
    let new_path = &event.paths[1];
    displayed_dirs
        .iter()
        .filter(|directory| *directory == old_path)
        .map(|directory| (directory.clone(), new_path.clone()))
        .collect()
}

#[cfg(unix)]
fn directory_identity(path: &PathBuf) -> Option<DirectoryIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(path).ok()?;
    metadata.is_dir().then_some(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(not(unix))]
fn directory_identity(_path: &PathBuf) -> Option<DirectoryIdentity> {
    None
}

fn find_renamed_sibling(
    old_path: &PathBuf,
    expected_identity: DirectoryIdentity,
) -> Option<PathBuf> {
    let parent = old_path.parent()?;
    let entries = std::fs::read_dir(parent).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate == *old_path {
            continue;
        }
        if directory_identity(&candidate) == Some(expected_identity) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        affected_displayed_dirs, desired_watch_paths, directory_rename_pairs, event_is_relevant,
        find_renamed_sibling, RefreshDebouncer, MAX_BATCH_LATENCY, QUIET_PERIOD,
    };
    use notify::event::Flag;
    use notify::event::{AccessKind, CreateKind, ModifyKind, RenameMode};
    use notify::{Event, EventKind};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    fn event(kind: EventKind, paths: &[&str]) -> Event {
        let mut event = Event::new(kind);
        event.paths = paths.iter().map(PathBuf::from).collect();
        event
    }

    #[test]
    fn child_event_affects_only_its_displayed_parent() {
        let displayed = vec![PathBuf::from("/one"), PathBuf::from("/two")];
        let event = event(EventKind::Create(CreateKind::File), &["/one/new.txt"]);

        assert_eq!(
            affected_displayed_dirs(&event, &displayed),
            vec![displayed[0].clone()]
        );
    }

    #[test]
    fn nested_grandchild_does_not_refresh_a_non_recursive_view() {
        let displayed = vec![PathBuf::from("/one")];
        let event = event(EventKind::Create(CreateKind::File), &["/one/sub/new.txt"]);

        assert!(affected_displayed_dirs(&event, &displayed).is_empty());
    }

    #[test]
    fn access_events_are_ignored() {
        let event = event(EventKind::Access(AccessKind::Any), &["/one/file.txt"]);
        assert!(!event_is_relevant(&event));
    }

    #[test]
    fn close_after_write_event_is_relevant() {
        let event = event(
            EventKind::Access(AccessKind::Close(notify::event::AccessMode::Write)),
            &["/one/file.txt"],
        );
        assert!(event_is_relevant(&event));
    }

    #[test]
    fn rescan_event_is_relevant_even_without_paths() {
        let event = Event::new(EventKind::Other).set_flag(Flag::Rescan);
        assert!(event_is_relevant(&event));
        assert!(event.need_rescan());
    }

    #[test]
    fn rename_dirties_both_displayed_parents() {
        let displayed = vec![PathBuf::from("/one"), PathBuf::from("/two")];
        let event = event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["/one/file.txt", "/two/file.txt"],
        );
        let mut affected = affected_displayed_dirs(&event, &displayed);
        affected.sort();

        assert_eq!(affected, displayed);
    }

    #[test]
    fn displayed_directory_rename_pair_is_extracted() {
        let displayed = vec![PathBuf::from("/parent/old")];
        let event = event(
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            &["/parent/old", "/parent/new"],
        );

        assert_eq!(
            directory_rename_pairs(&event, &displayed),
            vec![(PathBuf::from("/parent/old"), PathBuf::from("/parent/new"))]
        );
    }

    #[test]
    fn renamed_sibling_is_found_by_directory_identity() {
        let root = std::env::temp_dir().join(format!(
            "file-explorer-watch-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let old_path = root.join("old");
        let new_path = root.join("new");
        std::fs::create_dir_all(&old_path).unwrap();
        let identity = super::directory_identity(&old_path).unwrap();
        std::fs::rename(&old_path, &new_path).unwrap();

        assert_eq!(find_renamed_sibling(&old_path, identity), Some(new_path));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn watch_paths_include_existing_directory_and_parent() {
        let root = std::env::temp_dir();
        let displayed = root.join("file-explorer-watch-target-does-not-need-to-exist");
        let paths = desired_watch_paths(&[displayed]);

        assert!(paths.contains(&root));
    }

    #[test]
    fn debounce_coalesces_duplicates_after_quiet_period() {
        let start = Instant::now();
        let directory = PathBuf::from("/one");
        let mut debounce = RefreshDebouncer::default();
        debounce.mark([directory.clone(), directory.clone()], start);

        assert!(debounce
            .take_due(start + QUIET_PERIOD - Duration::from_millis(1))
            .is_none());
        assert_eq!(
            debounce.take_due(start + QUIET_PERIOD),
            Some(vec![directory])
        );
    }

    #[test]
    fn debounce_has_a_maximum_latency_during_continuous_events() {
        let start = Instant::now();
        let mut debounce = RefreshDebouncer::default();
        debounce.mark([PathBuf::from("/one")], start);
        debounce.mark(
            [PathBuf::from("/two")],
            start + MAX_BATCH_LATENCY - Duration::from_millis(10),
        );

        assert!(debounce
            .take_due(start + MAX_BATCH_LATENCY - Duration::from_millis(1))
            .is_none());
        assert_eq!(
            debounce.take_due(start + MAX_BATCH_LATENCY),
            Some(vec![PathBuf::from("/one"), PathBuf::from("/two")])
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "manual backend smoke test; filesystem event timing is not deterministic in CI"]
    fn recommended_watcher_delivers_a_real_directory_event() {
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "file-explorer-watch-smoke-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
        std::fs::create_dir(&directory).unwrap();
        let mut watcher = super::DirectoryWatcher::new(std::sync::Arc::new(|| {})).unwrap();
        let displayed = vec![directory.clone()];
        assert!(watcher.sync_paths(&displayed, Instant::now()).is_empty());
        std::thread::sleep(Duration::from_secs(2));
        std::fs::write(directory.join("created.txt"), "created").unwrap();

        let timeout = Instant::now() + Duration::from_secs(10);
        let refreshed = loop {
            let now = Instant::now();
            let _ = watcher.drain_events(&displayed, now);
            if let Some(dirs) = watcher.take_due_refresh(now) {
                break dirs;
            }
            assert!(now < timeout, "timed out waiting for a filesystem event");
            std::thread::sleep(Duration::from_millis(20));
        };

        assert_eq!(refreshed, displayed);
        drop(watcher);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

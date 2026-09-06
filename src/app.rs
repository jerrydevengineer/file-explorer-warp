use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::core::terminal::TerminalState;
use crate::core::{
    bookmarks::Bookmarks,
    config::AppConfig,
    display_text,
    fs::{read_dir_checked, sort_entries, FileEntry},
    fs_watch::DirectoryWatcher,
    global_tags::GlobalTags,
    search::SearchEngine,
    session::{
        session_path, PaneSession, SavedPaneSide, SessionState, TabSession, SESSION_VERSION,
    },
};
use crate::git::{diff as git_diff, graph as git_graph, operations as git_ops, repo as git_repo};
use crate::platform::{clipboard, opener, quicklook, share};
use crate::ui::{
    file_list::{self, FileListAction, FileListState},
    git_panel::{self, GitPanelAction, GitPanelState},
    prefs, search_overlay,
    sidebar::{self, SidebarAction},
    tab_bar,
    terminal_panel::{self, TerminalPanelEvent},
    toasts::Toasts,
};

// ── Per-tab state ─────────────────────────────────────────────────────────────

pub struct TabState {
    pub current_path: PathBuf,
    pub entries: Vec<FileEntry>,
    pub list_state: FileListState,
    pub tag_filter: Option<String>,
    pub tag_search_results: Option<Vec<PathBuf>>,
    history: Vec<PathBuf>,
    history_pos: usize,
}

#[derive(Debug)]
pub enum ReloadOutcome {
    Reloaded,
    CurrentDirectoryMissing,
    Failed(String),
}

impl TabState {
    pub fn new(path: PathBuf, show_hidden: bool) -> Self {
        let mut tab = Self {
            current_path: path.clone(),
            entries: Vec::new(),
            list_state: FileListState::default(),
            tag_filter: None,
            tag_search_results: None,
            history: vec![path],
            history_pos: 0,
        };
        tab.reload(show_hidden);
        tab
    }

    pub fn reload(&mut self, show_hidden: bool) -> ReloadOutcome {
        let selected = self.list_state.selected.clone();
        let anchor = self.list_state.selection_anchor.clone();
        let mut entries = match read_dir_checked(&self.current_path, show_hidden) {
            Ok(entries) => entries,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound || !self.current_path.is_dir() =>
            {
                return ReloadOutcome::CurrentDirectoryMissing;
            }
            Err(error) => return ReloadOutcome::Failed(error.to_string()),
        };
        sort_entries(
            &mut entries,
            self.list_state.sort_col,
            self.list_state.sort_order,
        );
        self.entries = entries;
        self.list_state.selected = selected
            .into_iter()
            .filter(|path| self.entries.iter().any(|entry| &entry.path == path))
            .collect();
        self.list_state.selection_anchor =
            anchor.filter(|path| self.entries.iter().any(|entry| &entry.path == path));
        if self
            .list_state
            .renaming
            .as_ref()
            .is_some_and(|renaming| !renaming.path.exists())
        {
            self.list_state.renaming = None;
        }
        ReloadOutcome::Reloaded
    }

    fn reload_after_path_change(&mut self, show_hidden: bool) {
        if !matches!(self.reload(show_hidden), ReloadOutcome::Reloaded) {
            self.entries.clear();
            self.list_state.clear_selection();
        }
    }

    pub fn navigate(&mut self, path: PathBuf, show_hidden: bool) -> bool {
        if path.is_dir() {
            // Discard any forward history when navigating to a new location.
            self.history.truncate(self.history_pos + 1);
            self.history.push(path.clone());
            self.history_pos = self.history.len() - 1;
            self.current_path = path;
            self.set_tag_view(None, None);
            self.reload_after_path_change(show_hidden);
            true
        } else {
            false
        }
    }

    fn visible_entry_paths(&self) -> Vec<PathBuf> {
        if let Some(results) = &self.tag_search_results {
            return results.clone();
        }
        self.entries
            .iter()
            .filter(|entry| {
                self.tag_filter.as_ref().map_or(true, |filter| {
                    entry.tags.iter().any(|tag| &tag.name == filter)
                })
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    fn set_tag_view(&mut self, tag: Option<String>, search_results: Option<Vec<PathBuf>>) {
        self.tag_filter = tag;
        self.tag_search_results = search_results;
        self.list_state.renaming = None;
        self.list_state.creating = None;
        self.list_state.clear_selection();
    }

    pub fn can_go_back(&self) -> bool {
        self.history_pos > 0
    }

    pub fn go_back(&mut self, show_hidden: bool) {
        if self.history_pos > 0 {
            self.history_pos -= 1;
            self.current_path = self.history[self.history_pos].clone();
            self.set_tag_view(None, None);
            self.reload_after_path_change(show_hidden);
        }
    }

    pub fn name(&self) -> String {
        if self.current_path.file_name().is_some() {
            display_text::file_name(&self.current_path)
        } else {
            "/".to_string()
        }
    }

    fn follow_external_directory_rename(
        &mut self,
        old_path: &PathBuf,
        new_path: &PathBuf,
        show_hidden: bool,
    ) -> bool {
        if self.current_path != *old_path || !new_path.is_dir() {
            return false;
        }

        self.current_path = new_path.clone();
        if let Some(history_path) = self.history.get_mut(self.history_pos) {
            *history_path = new_path.clone();
        }
        for selected in &mut self.list_state.selected {
            *selected = remap_path_prefix(selected, old_path, new_path);
        }
        self.list_state.selection_anchor = self
            .list_state
            .selection_anchor
            .as_ref()
            .map(|path| remap_path_prefix(path, old_path, new_path));
        if let Some(renaming) = &mut self.list_state.renaming {
            renaming.path = remap_path_prefix(&renaming.path, old_path, new_path);
        }
        self.reload_after_path_change(show_hidden);
        true
    }

    fn recover_missing_directory(&mut self, show_hidden: bool) -> Option<(PathBuf, PathBuf)> {
        if self.current_path.is_dir() {
            return None;
        }
        let old_path = self.current_path.clone();
        let fallback = nearest_existing_directory(&old_path)?;
        self.current_path = fallback.clone();
        if let Some(history_path) = self.history.get_mut(self.history_pos) {
            *history_path = fallback.clone();
        }
        self.set_tag_view(None, None);
        let _ = self.reload(show_hidden);
        Some((old_path, fallback))
    }
}

// ── Pane (one tab group) ──────────────────────────────────────────────────────

pub struct PaneState {
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
}

impl PaneState {
    pub fn new(path: PathBuf, show_hidden: bool) -> Self {
        Self {
            tabs: vec![TabState::new(path, show_hidden)],
            active_tab: 0,
        }
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn tab_names(&self) -> Vec<String> {
        self.tabs.iter().map(|t| t.name()).collect()
    }

    pub fn new_tab(&mut self, show_hidden: bool) {
        let path = self.tabs[self.active_tab].current_path.clone();
        let tab = TabState::new(path, show_hidden);
        self.active_tab += 1;
        self.tabs.insert(self.active_tab, tab);
    }

    /// Returns true if the pane should be removed (last tab closed).
    pub fn close_tab(&mut self, idx: usize) -> bool {
        if self.tabs.len() == 1 {
            return true;
        }
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        false
    }

    /// Remove and return tab at `idx`. Returns None if it's the last tab.
    pub fn take_tab(&mut self, idx: usize) -> Option<TabState> {
        if self.tabs.len() == 1 {
            return None;
        }
        let tab = self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        Some(tab)
    }

    pub fn add_tab(&mut self, tab: TabState) {
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn reload_all(&mut self, show_hidden: bool) {
        for t in &mut self.tabs {
            t.reload(show_hidden);
        }
    }
}

// ── File clipboard ────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClipboardKind {
    Copy,
    Cut,
}

#[derive(Clone)]
struct ClipboardOp {
    kind: ClipboardKind,
    paths: Vec<PathBuf>,
    change_count: i64,
}

#[derive(Default)]
struct PasteOutcome {
    created: Vec<PathBuf>,
    succeeded_sources: Vec<PathBuf>,
    moved_sources: Vec<PathBuf>,
    failed: Vec<(PathBuf, String)>,
    destination_changed: bool,
}

fn copy_path_recursive(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            copy_path_recursive(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

fn move_path(from: &std::path::Path, to_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let file_name = from.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "move is prohibited because the source has no file name",
        )
    })?;
    let dest = to_dir.join(file_name);

    let same_canonical_location = if dest.exists() {
        match (std::fs::canonicalize(from), std::fs::canonicalize(&dest)) {
            (Ok(source), Ok(destination)) => source == destination,
            _ => false,
        }
    } else {
        false
    };
    let same_location = from == dest || same_canonical_location;
    if same_location {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "move is prohibited because '{}' is already in '{}'",
                display_text::os_str(file_name),
                display_text::path(to_dir),
            ),
        ));
    }

    if from.is_dir() {
        let canonical_source = std::fs::canonicalize(from).unwrap_or_else(|_| from.to_path_buf());
        let canonical_target =
            std::fs::canonicalize(to_dir).unwrap_or_else(|_| to_dir.to_path_buf());
        if canonical_target.starts_with(&canonical_source) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "move is prohibited because a folder cannot be moved into itself or one of its subfolders",
            ));
        }
    }

    if dest.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!(
                "move is prohibited because the destination already contains '{}'",
                display_text::os_str(file_name),
            ),
        ));
    }

    std::fs::rename(from, &dest).or_else(|_| -> std::io::Result<()> {
        copy_path_recursive(from, &dest)?;
        if from.is_dir() {
            std::fs::remove_dir_all(from)
        } else {
            std::fs::remove_file(from)
        }
    })?;
    Ok(dest)
}

fn move_error_message(from: &std::path::Path, error: &std::io::Error) -> String {
    if matches!(
        error.kind(),
        std::io::ErrorKind::InvalidInput | std::io::ErrorKind::AlreadyExists
    ) {
        format!(
            "Move prohibited for {}: {}",
            display_text::path(from),
            error
        )
    } else {
        format!("Move failed for {}: {}", display_text::path(from), error)
    }
}

fn paste_paths(paths: &[PathBuf], kind: ClipboardKind, dest_dir: &std::path::Path) -> PasteOutcome {
    let mut outcome = PasteOutcome::default();
    for src in paths {
        let Some(file_name) = src.file_name() else {
            outcome
                .failed
                .push((src.clone(), "source has no file name".to_string()));
            continue;
        };
        let dest = dest_dir.join(file_name);
        if &dest == src {
            outcome.failed.push((
                src.clone(),
                "source and destination are the same".to_string(),
            ));
            continue;
        }
        if dest.exists() {
            outcome.failed.push((
                src.clone(),
                format!("{} already exists", display_text::path(&dest)),
            ));
            continue;
        }

        let result = match kind {
            ClipboardKind::Copy => copy_path_recursive(src, &dest),
            ClipboardKind::Cut => move_path(src, dest_dir).map(|_| ()),
        };
        match result {
            Ok(()) => {
                outcome.destination_changed = true;
                outcome.created.push(dest);
                outcome.succeeded_sources.push(src.clone());
                if kind == ClipboardKind::Cut {
                    outcome.moved_sources.push(src.clone());
                }
            }
            Err(error) => {
                if dest.exists() {
                    outcome.destination_changed = true;
                    outcome.created.push(dest);
                }
                outcome.failed.push((src.clone(), error.to_string()));
            }
        }
    }
    outcome
}

fn paste_reload_dirs(outcome: &PasteOutcome, dest_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if outcome.destination_changed {
        dirs.push(dest_dir.to_path_buf());
    }
    for source in &outcome.moved_sources {
        if let Some(parent) = source.parent() {
            let parent = parent.to_path_buf();
            if !dirs.contains(&parent) {
                dirs.push(parent);
            }
        }
    }
    dirs
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExternalDragResult {
    Cancelled,
    Copy,
    Move,
    Other(usize),
}

fn classify_external_drag_operation(operation: usize) -> ExternalDragResult {
    match operation {
        0 => ExternalDragResult::Cancelled,
        1 => ExternalDragResult::Copy,
        16 => ExternalDragResult::Move,
        other => ExternalDragResult::Other(other),
    }
}

fn external_drag_source_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for path in paths {
        if let Some(parent) = path.parent() {
            let parent = parent.to_path_buf();
            if !dirs.contains(&parent) {
                dirs.push(parent);
            }
        }
    }
    dirs
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

fn remap_path_prefix(path: &PathBuf, old_prefix: &PathBuf, new_prefix: &PathBuf) -> PathBuf {
    path.strip_prefix(old_prefix)
        .map(|suffix| new_prefix.join(suffix))
        .unwrap_or_else(|_| path.clone())
}

fn nearest_existing_directory(path: &PathBuf) -> Option<PathBuf> {
    let mut candidate = path.parent();
    while let Some(directory) = candidate {
        if directory.is_dir() {
            return Some(directory.to_path_buf());
        }
        candidate = directory.parent();
    }
    None
}

fn file_list_owns_keyboard_commands(
    wants_keyboard_input: bool,
    terminal_grid_has_focus: bool,
) -> bool {
    !wants_keyboard_input && !terminal_grid_has_focus
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabCycleDirection {
    Previous,
    Next,
}

fn cycle_tab_index(active_tab: usize, tab_count: usize, direction: TabCycleDirection) -> usize {
    if tab_count <= 1 {
        return 0;
    }

    let active_tab = active_tab.min(tab_count - 1);
    match direction {
        TabCycleDirection::Previous => {
            if active_tab == 0 {
                tab_count - 1
            } else {
                active_tab - 1
            }
        }
        TabCycleDirection::Next => (active_tab + 1) % tab_count,
    }
}

fn tab_cycle_key_event(event: &egui::Event) -> Option<(TabCycleDirection, bool)> {
    let egui::Event::Key {
        key,
        physical_key,
        pressed: true,
        repeat,
        modifiers,
    } = event
    else {
        return None;
    };

    if !modifiers.command || !modifiers.shift || modifiers.alt {
        return None;
    }

    let direction = if matches!(key, egui::Key::OpenCurlyBracket | egui::Key::OpenBracket)
        || matches!(physical_key, Some(egui::Key::OpenBracket))
    {
        TabCycleDirection::Previous
    } else if matches!(key, egui::Key::CloseCurlyBracket | egui::Key::CloseBracket)
        || matches!(physical_key, Some(egui::Key::CloseBracket))
    {
        TabCycleDirection::Next
    } else {
        return None;
    };

    Some((direction, *repeat))
}

/// Remove browser-tab cycle shortcuts before focused text widgets or the
/// embedded terminal can observe them. Repeated key-down events are consumed
/// but intentionally do not cycle again.
fn take_tab_cycle_shortcut(events: &mut Vec<egui::Event>) -> Option<TabCycleDirection> {
    let mut shortcut = None;
    events.retain(|event| {
        let Some((direction, repeat)) = tab_cycle_key_event(event) else {
            return true;
        };

        if !repeat && shortcut.is_none() {
            shortcut = Some(direction);
        }
        false
    });
    shortcut
}

// ── Focus / drag ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneSide {
    Left,
    Right,
}

const MAX_RESTORED_TABS_PER_PANE: usize = 64;
const SESSION_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
const SESSION_SAVE_RETRY: Duration = Duration::from_secs(5);

struct PendingSessionSave {
    snapshot: SessionState,
    deadline: Instant,
}

struct RestoredBrowserSession {
    left: PaneState,
    right: Option<PaneState>,
    focus: PaneSide,
    split_ratio: f32,
}

fn normalized_split_ratio(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.15, 0.85)
    } else {
        0.5
    }
}

fn pane_session(pane: &PaneState) -> PaneSession {
    PaneSession {
        tabs: pane
            .tabs
            .iter()
            .map(|tab| TabSession {
                path: tab.current_path.clone(),
            })
            .collect(),
        active_tab: pane.active_tab,
    }
}

fn browser_session_snapshot(
    left: &PaneState,
    right: Option<&PaneState>,
    focus: PaneSide,
    split_ratio: f32,
) -> SessionState {
    let saved_focus = if right.is_some() && focus == PaneSide::Right {
        SavedPaneSide::Right
    } else {
        SavedPaneSide::Left
    };
    SessionState {
        version: SESSION_VERSION,
        left: pane_session(left),
        right: right.map(pane_session),
        focus: saved_focus,
        split_ratio: normalized_split_ratio(split_ratio),
    }
}

fn resolve_saved_directory(path: &PathBuf, fallback: &PathBuf) -> PathBuf {
    if path.is_absolute() && path.is_dir() {
        return path.clone();
    }
    if path.is_absolute() {
        if let Some(directory) = nearest_existing_directory(path) {
            return directory;
        }
    }
    fallback.clone()
}

fn restore_pane(
    saved: &PaneSession,
    fallback: &PathBuf,
    show_hidden: bool,
) -> Option<PaneState> {
    let tabs: Vec<_> = saved
        .tabs
        .iter()
        .take(MAX_RESTORED_TABS_PER_PANE)
        .map(|tab| TabState::new(resolve_saved_directory(&tab.path, fallback), show_hidden))
        .collect();
    if tabs.is_empty() {
        return None;
    }
    let active_tab = saved.active_tab.min(tabs.len() - 1);
    Some(PaneState { tabs, active_tab })
}

fn restore_browser_session(
    saved: Option<&SessionState>,
    fallback: &PathBuf,
    show_hidden: bool,
) -> RestoredBrowserSession {
    let Some(saved) = saved else {
        return RestoredBrowserSession {
            left: PaneState::new(fallback.clone(), show_hidden),
            right: None,
            focus: PaneSide::Left,
            split_ratio: 0.5,
        };
    };

    let left = restore_pane(&saved.left, fallback, show_hidden)
        .unwrap_or_else(|| PaneState::new(fallback.clone(), show_hidden));
    let right = saved
        .right
        .as_ref()
        .and_then(|pane| restore_pane(pane, fallback, show_hidden));
    let focus = if right.is_some() && saved.focus == SavedPaneSide::Right {
        PaneSide::Right
    } else {
        PaneSide::Left
    };
    RestoredBrowserSession {
        left,
        right,
        focus,
        split_ratio: normalized_split_ratio(saved.split_ratio),
    }
}

fn update_pending_session(
    last_saved: Option<&SessionState>,
    pending: &mut Option<PendingSessionSave>,
    snapshot: SessionState,
    now: Instant,
) {
    if last_saved == Some(&snapshot) {
        *pending = None;
        return;
    }
    if pending
        .as_ref()
        .is_none_or(|pending| pending.snapshot != snapshot)
    {
        *pending = Some(PendingSessionSave {
            snapshot,
            deadline: now + SESSION_SAVE_DEBOUNCE,
        });
    }
}

struct TabDrag {
    from: PaneSide,
    tab_idx: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileDragState {
    paths: Vec<PathBuf>,
    origin_pane: PaneSide,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InternalFileDrop {
    paths: Vec<PathBuf>,
    destination: PathBuf,
}

fn resolve_internal_file_drop(
    explicit_target: Option<InternalFileDrop>,
    drag: Option<&FileDragState>,
    hovered_pane: Option<PaneSide>,
    left_directory: &PathBuf,
    right_directory: Option<&PathBuf>,
) -> Option<InternalFileDrop> {
    if explicit_target.is_some() {
        return explicit_target;
    }

    let drag = drag?;
    let destination = match (drag.origin_pane, hovered_pane) {
        (PaneSide::Right, Some(PaneSide::Left)) => left_directory.clone(),
        (PaneSide::Left, Some(PaneSide::Right)) => right_directory?.clone(),
        _ => return None,
    };
    Some(InternalFileDrop {
        paths: drag.paths.clone(),
        destination,
    })
}

fn internal_drop_reload_dirs(paths: &[PathBuf], destination: &PathBuf) -> Vec<PathBuf> {
    let mut directories = vec![destination.clone()];
    for path in paths {
        if let Some(parent) = path.parent() {
            push_unique_path(&mut directories, parent.to_path_buf());
        }
    }
    directories
}

#[derive(Default)]
struct FileActionOutcome {
    navigate_to: Option<PathBuf>,
    changed_dirs: Vec<PathBuf>,
    quicklook_path: Option<PathBuf>,
    select_after_nav: Option<PathBuf>,
    clear_tag_filter: bool,
    drag_started: Option<Vec<PathBuf>>,
    internal_drop: Option<InternalFileDrop>,
}

// ── Root app ──────────────────────────────────────────────────────────────────

pub struct App {
    config: AppConfig,
    bookmarks: Bookmarks,
    global_tags: GlobalTags,
    new_tag_input: String,
    new_tag_color: u8,
    edit_tag_idx: Option<usize>,
    edit_tag_name: String,
    edit_tag_color: u8,
    left: PaneState,
    right: Option<PaneState>,
    focus: PaneSide,
    tab_drag: Option<TabDrag>,
    file_drag: Option<FileDragState>,
    /// Fraction of content width taken by the left pane (0.15–0.85).
    split_ratio: f32,
    /// Content rect from last frame — used for tab-drag drop-zone detection.
    content_rect: egui::Rect,
    // ── Toasts / Prefs ────────────────────────────────────────────────────────
    toasts: Toasts,
    prefs_open: bool,
    // ── Clipboard ─────────────────────────────────────────────────────────────
    clipboard_op: Option<ClipboardOp>,
    // ── Search overlay ────────────────────────────────────────────────────────
    search_open: bool,
    search_just_opened: bool,
    search_query: String,
    search_engine: Option<SearchEngine>,
    search_results: Vec<PathBuf>,
    search_selected: usize,
    // ── Git panel ─────────────────────────────────────────────────────────────
    git_workdir: Option<PathBuf>,
    git_checked_path: Option<PathBuf>, // last path we ran detect_repo on
    git_panel_open: bool,
    git_panel: GitPanelState,
    // ── Filesystem auto-refresh ───────────────────────────────────────────────
    directory_watcher: Option<DirectoryWatcher>,
    // ── Browser session persistence ──────────────────────────────────────────
    last_saved_session: Option<SessionState>,
    pending_session_save: Option<PendingSessionSave>,
    session_save_error_logged: bool,
    // ── Terminal panel ────────────────────────────────────────────────────────
    terminal_open: bool,
    terminals: Vec<TerminalState>,
    terminal_active: usize,
    terminal_last_sync_path: Option<PathBuf>,
    // ── Custom themes ─────────────────────────────────────────────────────────
    custom_themes: Vec<crate::core::themes::CustomTheme>,
    // ── External drag ─────────────────────────────────────────────────────────
    #[cfg(target_os = "macos")]
    external_drag_active: bool,
    #[cfg(target_os = "macos")]
    next_external_drag_gesture_id: u64,
    #[cfg(target_os = "macos")]
    pending_external_drag_reload: Option<(std::time::Instant, Vec<std::path::PathBuf>)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let config = AppConfig::load();
        let custom_themes = crate::core::themes::load_themes();
        if let Some(ref id) = config.custom_theme {
            if let Some(t) = custom_themes.iter().find(|t| t.id == *id) {
                apply_custom_theme(&cc.egui_ctx, &t.colors);
            } else {
                apply_theme(&cc.egui_ctx, config.theme);
            }
        } else {
            apply_theme(&cc.egui_ctx, config.theme);
        }
        crate::platform::fonts::setup_fonts(&cc.egui_ctx);
        let bookmarks = Bookmarks::load();
        let start_path = config
            .last_path
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()))
            });
        let loaded_session = match SessionState::load() {
            Ok(session) => session,
            Err(error) => {
                eprintln!(
                    "[session] could not load {}: {error}",
                    session_path().display()
                );
                None
            }
        };
        let restored =
            restore_browser_session(loaded_session.as_ref(), &start_path, config.show_hidden);
        let initial_snapshot = browser_session_snapshot(
            &restored.left,
            restored.right.as_ref(),
            restored.focus,
            restored.split_ratio,
        );
        let last_saved_session = loaded_session.filter(|saved| saved == &initial_snapshot);
        let watcher_ctx = cc.egui_ctx.clone();
        let directory_watcher = DirectoryWatcher::new(Arc::new(move || {
            watcher_ctx.request_repaint();
        }))
        .map_err(|error| {
            eprintln!("[watch] auto-refresh unavailable: {error}");
            error
        })
        .ok();

        Self {
            left: restored.left,
            right: restored.right,
            focus: restored.focus,
            tab_drag: None,
            file_drag: None,
            split_ratio: restored.split_ratio,
            content_rect: egui::Rect::EVERYTHING,
            config,
            bookmarks,
            global_tags: GlobalTags::load(),
            new_tag_input: String::new(),
            new_tag_color: 6,
            edit_tag_idx: None,
            edit_tag_name: String::new(),
            edit_tag_color: 6,
            toasts: Toasts::default(),
            prefs_open: false,
            clipboard_op: None,
            search_open: false,
            search_just_opened: false,
            search_query: String::new(),
            search_engine: None,
            search_results: Vec::new(),
            search_selected: 0,
            git_workdir: None,
            git_checked_path: None,
            git_panel_open: false,
            git_panel: GitPanelState::default(),
            directory_watcher,
            last_saved_session,
            pending_session_save: None,
            session_save_error_logged: false,
            terminal_open: false,
            terminals: Vec::new(),
            terminal_active: 0,
            terminal_last_sync_path: None,
            custom_themes,
            #[cfg(target_os = "macos")]
            external_drag_active: false,
            #[cfg(target_os = "macos")]
            next_external_drag_gesture_id: 1,
            #[cfg(target_os = "macos")]
            pending_external_drag_reload: None,
        }
    }

    /// Reload all git panel data from disk.
    fn refresh_git(&mut self) {
        if let Some(wd) = &self.git_workdir.clone() {
            self.git_panel.status   = git_repo::load_status(wd);
            self.git_panel.branches = git_repo::load_branches(wd);
            self.git_panel.stashes  = git_repo::load_stashes(wd);
            self.git_panel.graph    = git_graph::build_graph(wd, 300);
            self.git_panel.diff     = None;
            self.git_panel.diff_file = None;
        }
    }

    fn toggle_terminal(&mut self, ctx: &egui::Context) {
        if self.terminal_open {
            self.terminal_open = false;
        } else {
            if self.terminals.is_empty() {
                let cwd = self.focused_pane().active().current_path.clone();
                let ctx2 = ctx.clone();
                match TerminalState::spawn(80, 24, &cwd, Arc::new(move || ctx2.request_repaint())) {
                    Ok(t) => {
                        self.terminals.push(t);
                        self.terminal_active = 0;
                    }
                    Err(_) => {
                        return;
                    }
                }
            }
            self.terminal_open = true;
            self.terminal_last_sync_path = None;
        }
    }

    fn focused_pane(&self) -> &PaneState {
        match self.focus {
            PaneSide::Left => &self.left,
            PaneSide::Right => self.right.as_ref().unwrap_or(&self.left),
        }
    }

    fn focused_pane_mut(&mut self) -> &mut PaneState {
        match self.focus {
            PaneSide::Left => &mut self.left,
            PaneSide::Right => self.right.as_mut().unwrap_or(&mut self.left),
        }
    }

    fn open_new_window() {
        if let Ok(exe) = std::env::current_exe() {
            let _ = std::process::Command::new(exe).spawn();
        }
    }

    fn move_tab_to_right(&mut self, tab_idx: usize) {
        if let Some(tab) = self.left.take_tab(tab_idx) {
            if let Some(r) = &mut self.right {
                r.add_tab(tab);
            } else {
                self.right = Some(PaneState {
                    tabs: vec![tab],
                    active_tab: 0,
                });
            }
        } else {
            // Last tab on left — clone the path to right (left must keep ≥1 tab)
            let path = self.left.tabs[0].current_path.clone();
            let hidden = self.config.show_hidden;
            let tab = TabState::new(path, hidden);
            if let Some(r) = &mut self.right {
                r.add_tab(tab);
            } else {
                self.right = Some(PaneState {
                    tabs: vec![tab],
                    active_tab: 0,
                });
            }
        }
        self.focus = PaneSide::Right;
    }

    fn move_tab_to_left(&mut self, tab_idx: usize) {
        if let Some(r) = &mut self.right {
            if let Some(tab) = r.take_tab(tab_idx) {
                self.left.add_tab(tab);
                if r.tabs.is_empty() {
                    self.right = None;
                }
            } else {
                // Last tab on right — clone to left, close right pane
                let path = r.tabs[0].current_path.clone();
                let hidden = self.config.show_hidden;
                self.left.add_tab(TabState::new(path, hidden));
                self.right = None;
            }
        }
        self.focus = PaneSide::Left;
    }

    fn handle_file_actions(
        actions: Vec<FileListAction>,
        bookmarks: &mut Bookmarks,
        toasts: &mut Toasts,
        terminal: crate::core::config::TerminalApp,
    ) -> FileActionOutcome {
        let mut outcome = FileActionOutcome::default();
        for action in actions {
            match action {
                FileListAction::Navigate(path) => outcome.navigate_to = Some(path),
                FileListAction::NavigateAndSelect(dir, file) => {
                    outcome.navigate_to = Some(dir);
                    outcome.select_after_nav = Some(file);
                }
                FileListAction::ClearTagFilter => {
                    outcome.clear_tag_filter = true;
                }
                FileListAction::OpenFile(path) => opener::open_file(&path),
                FileListAction::CopyPath(path) => {
                    clipboard::copy_path(&path);
                    toasts.push(format!("Copied: {}", display_text::path(&path)));
                }
                FileListAction::AddBookmark(path) => {
                    bookmarks.add(path);
                }
                FileListAction::RevealInFinder(path) => opener::reveal_in_finder(&path),
                FileListAction::OpenInTerminal(path) => opener::open_in_terminal(&path, terminal),
                FileListAction::DragStarted(paths) => {
                    outcome.drag_started = Some(paths);
                }
                FileListAction::QuickLook(path) => outcome.quicklook_path = Some(path),
                FileListAction::Share(path) => share::show_share_sheet(&path),
                FileListAction::GetInfo(path) => opener::get_info(&path),
                FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) => {}
                FileListAction::StartRename(_) | FileListAction::RenameItem(_, _) => {}
                FileListAction::CopyFiles(_)
                | FileListAction::CutFiles(_)
                | FileListAction::PasteHere => {}
                FileListAction::DeleteFiles(paths) => {
                    for path in paths {
                        let name = display_text::file_name(&path);
                        match opener::trash_path(&path) {
                            Ok(_) => {
                                toasts.push(format!("Moved to Trash: {}", name));
                                if let Some(parent) = path.parent() {
                                    push_unique_path(
                                        &mut outcome.changed_dirs,
                                        parent.to_path_buf(),
                                    );
                                }
                            }
                            Err(e) => {
                                toasts.push(format!(
                                    "Trash failed for {}: {}",
                                    display_text::path(&path),
                                    e
                                ));
                            }
                        }
                    }
                }
                FileListAction::MoveItems(paths, to_dir) => {
                    if outcome.internal_drop.is_none() {
                        outcome.internal_drop = Some(InternalFileDrop {
                            paths,
                            destination: to_dir,
                        });
                    }
                }
                FileListAction::SetTags(path, new_tags) => {
                    crate::core::tags::write_tags(&path, &new_tags);
                    if let Some(parent) = path.parent() {
                        push_unique_path(&mut outcome.changed_dirs, parent.to_path_buf());
                    }
                    let file_name = display_text::file_name(&path);
                    if new_tags.is_empty() {
                        toasts.push(format!("Removed all tags from {}", file_name));
                    } else {
                        let names: Vec<&str> = new_tags.iter().map(|t| t.name.as_str()).collect();
                        toasts.push(format!("{}: {}", file_name, names.join(", ")));
                    }
                }
            }
        }
        outcome
    }

    fn handle_creating_actions(
        actions: Vec<FileListAction>,
        tab: &mut TabState,
        toasts: &mut Toasts,
    ) -> (Vec<PathBuf>, Option<PathBuf>) {
        let mut changed_dirs = Vec::new();
        let mut select_after: Option<PathBuf> = None;
        for action in actions {
            match action {
                FileListAction::StartCreating(kind) => {
                    let name = match kind {
                        file_list::CreateKind::File => "untitled".to_string(),
                        file_list::CreateKind::Directory => "untitled folder".to_string(),
                    };
                    tab.list_state.creating = Some(file_list::CreatingItem {
                        kind,
                        name,
                        needs_focus: true,
                    });
                }
                FileListAction::CreateItem(kind, name) => {
                    let target = tab.current_path.join(&name);
                    match kind {
                        file_list::CreateKind::File => {
                            let _ = std::fs::File::create(&target);
                        }
                        file_list::CreateKind::Directory => {
                            let _ = std::fs::create_dir(&target);
                        }
                    }
                    push_unique_path(&mut changed_dirs, tab.current_path.clone());
                    select_after = Some(target);
                }
                FileListAction::StartRename(path) => {
                    let name = display_text::file_name(&path);
                    tab.list_state.select_only(path.clone());
                    tab.list_state.renaming = Some(file_list::RenamingItem {
                        path,
                        name,
                        needs_focus: true,
                    });
                }
                FileListAction::RenameItem(old_path, new_name) => {
                    // The editor shows NFC-normalized text. Treat an unchanged
                    // display name as a no-op so merely confirming an NFD name
                    // cannot silently change the on-disk path spelling.
                    if new_name == display_text::file_name(&old_path) {
                        continue;
                    }
                    let new_path = old_path.parent().unwrap_or(&old_path).join(&new_name);
                    match std::fs::rename(&old_path, &new_path) {
                        Ok(_) => {
                            push_unique_path(&mut changed_dirs, tab.current_path.clone());
                            select_after = Some(new_path);
                        }
                        Err(e) => {
                            toasts.push(format!("Rename failed: {}", e));
                        }
                    }
                }
                _ => {}
            }
        }
        (changed_dirs, select_after)
    }

    fn set_file_clipboard(
        kind: ClipboardKind,
        paths: Vec<PathBuf>,
        clipboard_op: &mut Option<ClipboardOp>,
        toasts: &mut Toasts,
    ) {
        if paths.is_empty() {
            toasts.push("No files selected".to_string());
            return;
        }
        match clipboard::write_files(&paths) {
            Ok(change_count) => {
                let count = paths.len();
                *clipboard_op = Some(ClipboardOp {
                    kind,
                    paths,
                    change_count,
                });
                let verb = if kind == ClipboardKind::Cut {
                    "Cut"
                } else {
                    "Copied"
                };
                toasts.push(format!(
                    "{} {} item{}",
                    verb,
                    count,
                    if count == 1 { "" } else { "s" }
                ));
            }
            Err(error) => toasts.push(format!("Clipboard failed: {}", error)),
        }
    }

    fn do_paste_op(
        dest_dir: &PathBuf,
        clipboard_op: &mut Option<ClipboardOp>,
        toasts: &mut Toasts,
    ) -> PasteOutcome {
        let system_clipboard = match clipboard::read_files() {
            Ok(contents) => contents,
            Err(_) => {
                toasts.push("Clipboard does not contain files".to_string());
                return PasteOutcome::default();
            }
        };
        let internal_matches = clipboard_op.as_ref().map_or(false, |op| {
            op.change_count == system_clipboard.change_count && op.paths == system_clipboard.paths
        });
        let kind = if internal_matches {
            clipboard_op
                .as_ref()
                .map_or(ClipboardKind::Copy, |op| op.kind)
        } else {
            ClipboardKind::Copy
        };

        let outcome = paste_paths(&system_clipboard.paths, kind, dest_dir);
        if !outcome.succeeded_sources.is_empty() {
            let verb = if kind == ClipboardKind::Cut {
                "Moved"
            } else {
                "Pasted"
            };
            toasts.push(format!(
                "{} {} item{}",
                verb,
                outcome.succeeded_sources.len(),
                if outcome.succeeded_sources.len() == 1 {
                    ""
                } else {
                    "s"
                },
            ));
        }
        for (path, error) in &outcome.failed {
            toasts.push(format!(
                "Paste failed for {}: {}",
                display_text::path(path),
                error
            ));
        }

        if kind == ClipboardKind::Cut && internal_matches {
            if let Some(op) = clipboard_op.as_mut() {
                op.paths
                    .retain(|path| !outcome.succeeded_sources.contains(path));
                if op.paths.is_empty() {
                    clipboard::clear_files();
                    *clipboard_op = None;
                } else {
                    match clipboard::write_files(&op.paths) {
                        Ok(change_count) => op.change_count = change_count,
                        Err(error) => {
                            toasts.push(format!("Could not update remaining cut items: {}", error));
                            *clipboard_op = None;
                        }
                    }
                }
            }
        } else if !internal_matches {
            *clipboard_op = None;
        }
        outcome
    }

    fn handle_clipboard_actions(
        actions: Vec<FileListAction>,
        clipboard_op: &mut Option<ClipboardOp>,
        paste_dir: &PathBuf,
        toasts: &mut Toasts,
    ) -> PasteOutcome {
        let mut outcome = PasteOutcome::default();
        for action in actions {
            match action {
                FileListAction::CopyFiles(paths) => {
                    Self::set_file_clipboard(ClipboardKind::Copy, paths, clipboard_op, toasts)
                }
                FileListAction::CutFiles(paths) => {
                    Self::set_file_clipboard(ClipboardKind::Cut, paths, clipboard_op, toasts)
                }
                FileListAction::PasteHere => {
                    outcome = Self::do_paste_op(paste_dir, clipboard_op, toasts);
                }
                _ => {}
            }
        }
        outcome
    }

    fn reload_after_paste(&mut self, outcome: &PasteOutcome, dest_dir: &PathBuf) {
        let dirs = paste_reload_dirs(outcome, dest_dir);
        self.reload_tabs_in_dirs(&dirs);
    }

    fn reload_tabs_in_dirs(&mut self, dirs: &[PathBuf]) {
        if dirs.is_empty() {
            return;
        }
        let show_hidden = self.config.show_hidden;
        let mut fallbacks = Vec::new();
        let mut errors = Vec::new();
        for tab in &mut self.left.tabs {
            if dirs.contains(&tab.current_path) {
                match tab.reload(show_hidden) {
                    ReloadOutcome::Reloaded => {}
                    ReloadOutcome::CurrentDirectoryMissing => {
                        if let Some(fallback) = tab.recover_missing_directory(show_hidden) {
                            if !fallbacks.contains(&fallback) {
                                fallbacks.push(fallback);
                            }
                        }
                    }
                    ReloadOutcome::Failed(error) => {
                        errors.push((tab.current_path.clone(), error));
                    }
                }
            }
        }
        if let Some(right) = &mut self.right {
            for tab in &mut right.tabs {
                if dirs.contains(&tab.current_path) {
                    match tab.reload(show_hidden) {
                        ReloadOutcome::Reloaded => {}
                        ReloadOutcome::CurrentDirectoryMissing => {
                            if let Some(fallback) = tab.recover_missing_directory(show_hidden) {
                                if !fallbacks.contains(&fallback) {
                                    fallbacks.push(fallback);
                                }
                            }
                        }
                        ReloadOutcome::Failed(error) => {
                            errors.push((tab.current_path.clone(), error));
                        }
                    }
                }
            }
        }
        for (old_path, fallback) in fallbacks {
            self.toasts.push(format!(
                "Folder moved or removed: {}\nShowing: {}",
                display_text::path(&old_path),
                display_text::path(&fallback),
            ));
        }
        for (path, error) in errors {
            eprintln!("[watch] could not refresh {}: {error}", path.display());
        }
    }

    fn perform_internal_file_drop(&mut self, drop: InternalFileDrop) {
        let changed_dirs = internal_drop_reload_dirs(&drop.paths, &drop.destination);
        for source in drop.paths {
            match move_path(&source, &drop.destination) {
                Ok(_) => self
                    .toasts
                    .push(format!("Moved: {}", display_text::file_name(&source))),
                Err(error) => self.toasts.push(move_error_message(&source, &error)),
            }
        }
        self.reload_tabs_in_dirs(&changed_dirs);
    }

    fn displayed_directories(&self) -> Vec<PathBuf> {
        let mut directories = Vec::new();
        for tab in &self.left.tabs {
            push_unique_path(&mut directories, tab.current_path.clone());
        }
        if let Some(right) = &self.right {
            for tab in &right.tabs {
                push_unique_path(&mut directories, tab.current_path.clone());
            }
        }
        directories
    }

    fn follow_external_directory_renames(&mut self, renames: &[(PathBuf, PathBuf)]) {
        if renames.is_empty() {
            return;
        }
        let show_hidden = self.config.show_hidden;
        for (old_path, new_path) in renames {
            if let Some(drag) = &mut self.file_drag {
                for path in &mut drag.paths {
                    *path = remap_path_prefix(path, old_path, new_path);
                }
            }
            let mut followed = false;
            for tab in &mut self.left.tabs {
                followed |= tab.follow_external_directory_rename(old_path, new_path, show_hidden);
            }
            if let Some(right) = &mut self.right {
                for tab in &mut right.tabs {
                    followed |=
                        tab.follow_external_directory_rename(old_path, new_path, show_hidden);
                }
            }
            if followed {
                eprintln!(
                    "[watch] followed directory rename {} -> {}",
                    old_path.display(),
                    new_path.display(),
                );
            }
        }
    }

    fn process_directory_watcher(&mut self, ctx: &egui::Context) {
        let displayed_dirs = self.displayed_directories();
        let now = std::time::Instant::now();
        let Some(watcher) = &mut self.directory_watcher else {
            return;
        };

        let update = watcher.drain_events(&displayed_dirs, now);
        let mut errors = update.errors;
        errors.extend(watcher.sync_paths(&displayed_dirs, now));
        let refresh_dirs = watcher.take_due_refresh(now);
        let repaint_after = watcher.time_until_refresh(now);

        for error in errors {
            eprintln!("[watch] {error}");
        }
        self.follow_external_directory_renames(&update.directory_renames);
        if let Some(dirs) = refresh_dirs {
            self.reload_tabs_in_dirs(&dirs);
        }
        if let Some(delay) = repaint_after {
            ctx.request_repaint_after(delay);
        }
    }

    fn session_snapshot(&self) -> SessionState {
        browser_session_snapshot(
            &self.left,
            self.right.as_ref(),
            self.focus,
            self.split_ratio,
        )
    }

    fn process_session_persistence(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let snapshot = self.session_snapshot();
        update_pending_session(
            self.last_saved_session.as_ref(),
            &mut self.pending_session_save,
            snapshot,
            now,
        );

        let Some(pending) = self.pending_session_save.as_ref() else {
            return;
        };
        if now < pending.deadline {
            ctx.request_repaint_after(pending.deadline.saturating_duration_since(now));
            return;
        }

        let pending = self.pending_session_save.take().unwrap();
        match pending.snapshot.save() {
            Ok(()) => {
                self.last_saved_session = Some(pending.snapshot);
                self.session_save_error_logged = false;
            }
            Err(error) => {
                if !self.session_save_error_logged {
                    eprintln!(
                        "[session] could not save {}: {error}",
                        session_path().display()
                    );
                    self.session_save_error_logged = true;
                }
                self.pending_session_save = Some(PendingSessionSave {
                    snapshot: pending.snapshot,
                    deadline: now + SESSION_SAVE_RETRY,
                });
                ctx.request_repaint_after(SESSION_SAVE_RETRY);
            }
        }
    }

    fn save_session_now(&mut self) {
        let snapshot = self.session_snapshot();
        if self.last_saved_session.as_ref() == Some(&snapshot) {
            self.pending_session_save = None;
            return;
        }
        match snapshot.save() {
            Ok(()) => {
                self.last_saved_session = Some(snapshot);
                self.pending_session_save = None;
                self.session_save_error_logged = false;
            }
            Err(error) => {
                eprintln!(
                    "[session] could not save {} during shutdown: {error}",
                    session_path().display()
                );
            }
        }
    }

    fn select_paste_results(&mut self, created: &[PathBuf], dest_dir: &PathBuf) {
        if self.focused_pane().active().current_path == *dest_dir {
            let existing: Vec<PathBuf> = created
                .iter()
                .filter(|path| path.exists())
                .cloned()
                .collect();
            if !existing.is_empty() {
                self.focused_pane_mut()
                    .active_mut()
                    .list_state
                    .select_all(existing.iter());
            }
        }
    }

    fn do_quicklook(&mut self, path: PathBuf) {
        quicklook::open_quicklook(&path);
    }

    fn handle_git_actions(&mut self, actions: Vec<GitPanelAction>, wd: &PathBuf) {
        let mut needs_refresh = false;
        let mut needs_dir_reload = false;

        for action in actions {
            match action {
                GitPanelAction::Refresh => {
                    needs_refresh = true;
                }
                GitPanelAction::SwitchTab(_) => {}

                GitPanelAction::StageFile(path) => match git_ops::stage_file(wd, &path) {
                    Ok(_) => {
                        needs_refresh = true;
                    }
                    Err(e) => {
                        self.toasts.push(format!("Stage failed: {}", e));
                }
                },
                GitPanelAction::UnstageFile(path) => match git_ops::unstage_file(wd, &path) {
                    Ok(_) => {
                        needs_refresh = true;
                    }
                    Err(e) => {
                        self.toasts.push(format!("Unstage failed: {}", e));
                }
                },
                GitPanelAction::StageAll => match git_ops::stage_all(wd) {
                    Ok(_) => {
                        needs_refresh = true;
                    }
                    Err(e) => {
                        self.toasts.push(format!("Stage all failed: {}", e));
                }
                },
                GitPanelAction::UnstageAll => match git_ops::unstage_all(wd) {
                    Ok(_) => {
                        needs_refresh = true;
                    }
                    Err(e) => {
                        self.toasts.push(format!("Unstage all failed: {}", e));
                }
                },
                GitPanelAction::SelectDiff(path, staged) => {
                    self.git_panel.diff = git_diff::get_file_diff(wd, &path, staged);
                    self.git_panel.diff_file = Some((path, staged));
                }
                GitPanelAction::Commit(msg) => match git_ops::commit(wd, &msg) {
                        Ok(_) => {
                            self.git_panel.commit_msg.clear();
                            self.toasts.push("Committed.");
                            needs_refresh = true;
                        }
                    Err(e) => {
                        self.toasts.push(format!("Commit failed: {}", e));
                }
                },
                GitPanelAction::CheckoutBranch(name) => match git_ops::checkout_branch(wd, &name) {
                        Ok(_) => {
                            needs_refresh = true;
                            needs_dir_reload = true;
                            self.toasts.push(format!("Checked out '{}'", name));
                        }
                    Err(e) => {
                        self.toasts.push(format!("Checkout failed: {}", e));
                }
                },
                GitPanelAction::CreateBranch(name) => match git_ops::create_branch(wd, &name) {
                        Ok(_) => {
                            self.git_panel.new_branch_name.clear();
                            needs_refresh = true;
                            self.toasts.push(format!("Created branch '{}'", name));
                        }
                    Err(e) => {
                        self.toasts.push(format!("Create branch failed: {}", e));
                }
                },
                GitPanelAction::DeleteBranch(name) => match git_ops::delete_branch(wd, &name) {
                        Ok(_) => {
                            needs_refresh = true;
                            self.toasts.push(format!("Deleted branch '{}'", name));
                        }
                    Err(e) => {
                        self.toasts.push(format!("Delete branch failed: {}", e));
                }
                },
                GitPanelAction::StashSave => match git_ops::stash_save(wd) {
                        Ok(_) => {
                            needs_refresh = true;
                            needs_dir_reload = true;
                            self.toasts.push("Stashed working changes.");
                        }
                    Err(e) => {
                        self.toasts.push(format!("Stash failed: {}", e));
                }
                },
                GitPanelAction::StashApply(idx) => match git_ops::stash_apply(wd, idx) {
                        Ok(_) => {
                            needs_refresh = true;
                            needs_dir_reload = true;
                            self.toasts.push(format!("Applied stash [{}]", idx));
                        }
                    Err(e) => {
                        self.toasts.push(format!("Stash apply failed: {}", e));
                }
                },
                GitPanelAction::StashDrop(idx) => match git_ops::stash_drop(wd, idx) {
                        Ok(_) => {
                            needs_refresh = true;
                            self.toasts.push(format!("Dropped stash [{}]", idx));
                        }
                    Err(e) => {
                        self.toasts.push(format!("Stash drop failed: {}", e));
                }
                },
                GitPanelAction::Fetch => {
                    let remote = self.git_panel.remote_name.clone();
                    match git_ops::fetch(wd, &remote) {
                        Ok(msg) => {
                            self.git_panel
                                .op_log
                                .push(format!("fetch {}: {}", remote, msg));
                            needs_refresh = true;
                        }
                        Err(e) => {
                            self.git_panel.op_log.push(format!("fetch error: {}", e));
                        }
                    }
                }
                GitPanelAction::Pull => {
                    let remote = self.git_panel.remote_name.clone();
                    match git_ops::pull(wd, &remote) {
                        Ok(msg) => {
                            self.git_panel
                                .op_log
                                .push(format!("pull {}: {}", remote, msg));
                            needs_refresh = true;
                            needs_dir_reload = true;
                        }
                        Err(e) => {
                            self.git_panel.op_log.push(format!("pull error: {}", e));
                        }
                    }
                }
                GitPanelAction::Push => {
                    let remote = self.git_panel.remote_name.clone();
                    // Get current branch name from status
                    let branch = self
                        .git_panel
                        .status
                        .as_ref()
                        .and_then(|s| s.head_branch.clone())
                        .unwrap_or_else(|| "main".to_string());
                    match git_ops::push_branch(wd, &remote, &branch) {
                        Ok(msg) => {
                            self.git_panel
                                .op_log
                                .push(format!("push {}/{}: {}", remote, branch, msg));
                        }
                        Err(e) => {
                            self.git_panel.op_log.push(format!("push error: {}", e));
                        }
                    }
                }
                GitPanelAction::TogglePosition => {
                    self.config.git_panel_right = !self.config.git_panel_right;
                    self.config.save();
                }
                GitPanelAction::LoadConfig => {
                    self.git_panel.config_global = git_repo::load_git_config(wd, true);
                    self.git_panel.config_local = git_repo::load_git_config(wd, false);
                    self.git_panel.config_loaded = true;
                }
            }
        }

        if needs_refresh {
            self.refresh_git();
        }
        if needs_dir_reload {
            let h = self.config.show_hidden;
            self.focused_pane_mut().active_mut().reload(h);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_directory_watcher(ctx);
        let pointer_released = ctx.input(|i| i.pointer.any_released());

        let missing_drag_paths = self.file_drag.as_ref().map_or(0, |drag| {
            drag.paths.iter().filter(|path| !path.exists()).count()
        });
        if missing_drag_paths > 0 {
            self.toasts.push(format!(
                "Cancelled drag because {} source item{} no longer exist{}",
                missing_drag_paths,
                if missing_drag_paths == 1 { "" } else { "s" },
                if missing_drag_paths == 1 { "s" } else { "" },
            ));
            crate::platform::drag::cancel_pending_external_drag();
            self.file_drag = None;
        }

        // ── External drag-end reload ──────────────────────────────────────────
        #[cfg(target_os = "macos")]
        {
            let now_active = crate::platform::drag::is_drag_active();
            let just_ended = self.external_drag_active && !now_active;
            self.external_drag_active = now_active;

            if just_ended {
                ctx.request_repaint();
            }

            if let Some((operation, paths)) = crate::platform::drag::take_drag_ended_op() {
                let result = classify_external_drag_operation(operation);
                eprintln!("[app] external drag ended operation={operation} result={result:?}");
                match result {
                    ExternalDragResult::Move => {
                        // Give Finder time to finish its filesystem mutation before reload.
                        self.pending_external_drag_reload = Some((
                            std::time::Instant::now() + std::time::Duration::from_millis(300),
                            paths,
                        ));
                        ctx.request_repaint_after(std::time::Duration::from_millis(300));
                    }
                    ExternalDragResult::Cancelled | ExternalDragResult::Copy => {}
                    ExternalDragResult::Other(other) => {
                        eprintln!("[app] ignoring unsupported external drag operation {other}");
                    }
                }
            }

            let reload_due = self
                .pending_external_drag_reload
                .as_ref()
                .map_or(false, |(d, _)| std::time::Instant::now() >= *d);
            if reload_due {
                if let Some((_, paths)) = self.pending_external_drag_reload.take() {
                    let h = self.config.show_hidden;
                    // Reload every tab (in both panes) that is showing the directory
                    // from which files were dragged — not just whichever tab is active now.
                    let source_dirs = external_drag_source_dirs(&paths);
                    for tab in &mut self.left.tabs {
                        if source_dirs.iter().any(|d| d == &tab.current_path) {
                            tab.reload(h);
                        }
                    }
                    if let Some(r) = &mut self.right {
                        for tab in &mut r.tabs {
                            if source_dirs.iter().any(|d| d == &tab.current_path) {
                                tab.reload(h);
                            }
                        }
                    }
                }
            }
        }

        // ── File drag ghost ───────────────────────────────────────────────────
        let file_dragging_name = self
            .file_drag
            .as_ref()
            .map(|drag| {
                if drag.paths.len() == 1 {
                    display_text::file_name(&drag.paths[0])
                } else {
                    format!("{} items", drag.paths.len())
                }
            });

        if let Some(name) = &file_dragging_name {
            if let Some(pos) = ctx.pointer_hover_pos() {
                egui::show_tooltip_at(
                    ctx,
                    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag_ghost")),
                    egui::Id::new("drag_ghost"),
                    pos + egui::vec2(12.0, 4.0),
                    |ui| {
                        ui.label(format!("📁 {}", name));
                    },
                );
            }
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        }

        // ── Tab drag ghost + drop detection ───────────────────────────────────
        if let Some(drag) = &self.tab_drag {
            let pane = match drag.from {
                PaneSide::Left => &self.left,
                PaneSide::Right => self.right.as_ref().unwrap_or(&self.left),
            };
            let name = pane
                .tabs
                .get(drag.tab_idx)
                .map(|t| t.name())
                .unwrap_or_default();
            if let Some(pos) = ctx.pointer_hover_pos() {
                egui::show_tooltip_at(
                    ctx,
                    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("tab_drag_ghost")),
                    egui::Id::new("tab_drag_ghost"),
                    pos + egui::vec2(12.0, 4.0),
                    |ui| {
                        ui.label(format!("⬜ {}", name));
                    },
                );
            }
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);

            if pointer_released {
                if let Some(pos) = ctx.pointer_hover_pos() {
                    let in_right_zone = pos.x > self.content_rect.center().x;
                    let from = drag.from;
                    let tab_idx = drag.tab_idx;
                    match from {
                        PaneSide::Left if in_right_zone => self.move_tab_to_right(tab_idx),
                        PaneSide::Right if !in_right_zone => self.move_tab_to_left(tab_idx),
                        _ => {} // dropped back on same side — no-op
                    }
                }
                self.tab_drag = None;
            }
        }

        // ── Keyboard shortcuts ────────────────────────────────────────────────
        let mut kb_new_tab = false;
        let mut kb_close_tab = false;
        let mut kb_close_right = false;
        let mut kb_new_window = false;
        let mut kb_go_up = false;
        let mut kb_reload = false;
        let mut kb_toggle_hidden = false;
        let mut kb_switch_focus = false;
        let mut kb_switch_tab: Option<usize> = None;
        let mut kb_cycle_tab: Option<TabCycleDirection> = None;
        let mut kb_open_search = false;
        let mut kb_toggle_git = false;
        let mut kb_toggle_terminal = false;
        let mut kb_open_prefs = false;
        let mut kb_quicklook = false;
        let mut kb_copy_file = false;
        let mut kb_cut_file = false;
        let mut kb_paste_file = false;
        let mut kb_delete_file = false;
        let mut kb_select_all = false;
        let mut kb_move_selection: Option<(isize, bool)> = None;

        // Bare-key shortcuts (no modifier) must not fire while a text field has focus,
        // otherwise Backspace in a TextEdit would also navigate up a directory.
        // wants_keyboard_input() is true only when a TextEdit is actively receiving text,
        // NOT just because an interactive widget (like a file row) has focus.
        // Additionally check if the terminal grid has focus: it uses a custom Sense
        // (not a TextEdit) so wants_keyboard_input() is blind to it, but bare-key
        // shortcuts (Backspace → go_up, Space → quicklook) must not fire there either.
        let terminal_has_focus = self.terminal_open && !self.terminals.is_empty() && {
            let terminal_grid_id = egui::Id::new("terminal_panel").with("terminal_grid");
            ctx.memory(|m| m.has_focus(terminal_grid_id))
        };
        let file_shortcuts_active =
            file_list_owns_keyboard_commands(ctx.wants_keyboard_input(), terminal_has_focus);

        ctx.input(|i| {
            if i.modifiers.command && i.key_pressed(egui::Key::T) {
                kb_new_tab = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::W) {
                kb_close_tab = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::Backslash) {
                kb_close_right = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::F) {
                kb_open_search = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::G) {
                kb_toggle_git = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::J) {
                kb_toggle_terminal = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::N) {
                kb_new_window = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::Comma) {
                kb_open_prefs = true;
            }
            // Backspace = go up; Cmd+Backspace = move to Trash
            if file_shortcuts_active && i.key_pressed(egui::Key::Backspace) {
                if i.modifiers.command {
                    kb_delete_file = true;
                } else {
                    kb_go_up = true;
                }
            }
            // egui-winit converts Cmd+C/X/V on macOS to Event::Copy/Cut/Paste,
            // so key_pressed(Key::C) never fires. Check the semantic events instead.
            for event in &i.events {
                match event {
                    egui::Event::Copy if file_shortcuts_active => kb_copy_file = true,
                    egui::Event::Cut if file_shortcuts_active => kb_cut_file = true,
                    egui::Event::Paste(_) if file_shortcuts_active => kb_paste_file = true,
                    egui::Event::Key {
                        key: egui::Key::V,
                        pressed: true,
                        modifiers,
                        ..
                    } if file_shortcuts_active && modifiers.command => kb_paste_file = true,
                    _ => {}
                }
            }
            if i.modifiers.command && i.key_pressed(egui::Key::R) {
                kb_reload = true;
            }
            if i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::Period) {
                kb_toggle_hidden = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::Backtick) {
                kb_switch_focus = true;
            }
            for n in 1..=9usize {
                let key = match n {
                    1 => egui::Key::Num1,
                    2 => egui::Key::Num2,
                    3 => egui::Key::Num3,
                    4 => egui::Key::Num4,
                    5 => egui::Key::Num5,
                    6 => egui::Key::Num6,
                    7 => egui::Key::Num7,
                    8 => egui::Key::Num8,
                    9 => egui::Key::Num9,
                    _ => unreachable!(),
                };
                if i.modifiers.command && i.key_pressed(key) {
                    kb_switch_tab = Some(n - 1);
                }
            }
        });

        // Space must be consumed via input_mut so the ScrollArea never sees it.
        ctx.input_mut(|i| {
            kb_cycle_tab = take_tab_cycle_shortcut(&mut i.events);
            if file_shortcuts_active && i.consume_key(egui::Modifiers::NONE, egui::Key::Space) {
                kb_quicklook = true;
            }
            if file_shortcuts_active && i.consume_key(egui::Modifiers::COMMAND, egui::Key::A) {
                kb_select_all = true;
            }
            // egui's logical modifier matching lets an unmodified shortcut match
            // extra Shift/Alt modifiers, so always consume the specific variants first.
            if file_shortcuts_active && i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowUp) {
                kb_move_selection = Some((-1, true));
            } else if file_shortcuts_active
                && i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowDown)
            {
                kb_move_selection = Some((1, true));
            } else if file_shortcuts_active
                && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
            {
                kb_move_selection = Some((-1, false));
            } else if file_shortcuts_active
                && i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
            {
                kb_move_selection = Some((1, false));
            }
        });

        if kb_select_all {
            let paths = self.focused_pane().active().visible_entry_paths();
            self.focused_pane_mut()
                .active_mut()
                .list_state
                .select_all(paths.iter());
        }
        if let Some((delta, extend)) = kb_move_selection {
            let paths = self.focused_pane().active().visible_entry_paths();
            self.focused_pane_mut()
                .active_mut()
                .list_state
                .move_primary(&paths, delta, extend);
        }

        if kb_quicklook {
            if let Some(path) = self
                .focused_pane()
                .active()
                .list_state
                .primary_selection()
                .cloned()
            {
                self.do_quicklook(path);
            }
        }
        if kb_copy_file {
            let paths = self.focused_pane().active().list_state.selected.clone();
            Self::set_file_clipboard(
                ClipboardKind::Copy,
                paths,
                &mut self.clipboard_op,
                &mut self.toasts,
            );
        }
        if kb_cut_file {
            let paths = self.focused_pane().active().list_state.selected.clone();
            Self::set_file_clipboard(
                ClipboardKind::Cut,
                paths,
                &mut self.clipboard_op,
                &mut self.toasts,
            );
        }
        if kb_paste_file {
            let dest_dir = self.focused_pane().active().current_path.clone();
            let outcome = Self::do_paste_op(&dest_dir, &mut self.clipboard_op, &mut self.toasts);
            self.reload_after_paste(&outcome, &dest_dir);
            self.select_paste_results(&outcome.created, &dest_dir);
        }
        if kb_delete_file {
            let paths = self.focused_pane().active().list_state.selected.clone();
            let mut changed_dirs = Vec::new();
            for path in paths {
                let name = display_text::file_name(&path);
                match opener::trash_path(&path) {
                    Ok(_) => {
                        self.toasts.push(format!("Moved to Trash: {}", name));
                        if let Some(parent) = path.parent() {
                            let parent = parent.to_path_buf();
                            if !changed_dirs.contains(&parent) {
                                changed_dirs.push(parent);
                            }
                        }
                    }
                    Err(e) => {
                        self.toasts.push(format!(
                            "Trash failed for {}: {}",
                            display_text::path(&path),
                            e
                        ));
                    }
                }
            }
            self.reload_tabs_in_dirs(&changed_dirs);
        }
        if kb_new_tab {
            let h = self.config.show_hidden;
            self.focused_pane_mut().new_tab(h);
        }
        if kb_close_tab {
            let focused = self.focus;
            let ai = self.focused_pane().active_tab;
            match focused {
                PaneSide::Left => {
                    self.left.close_tab(ai);
                }
                PaneSide::Right => {
                    if let Some(r) = &mut self.right {
                        if r.close_tab(ai) {
                            self.right = None;
                            self.focus = PaneSide::Left;
                        }
                    }
                }
            }
                    }
        if kb_close_right {
            self.right = None;
            self.focus = PaneSide::Left;
            self.tab_drag = None;
                }
        if kb_new_window {
            Self::open_new_window();
            }
        if kb_open_prefs {
            self.prefs_open = true;
        }
        if kb_toggle_hidden {
            self.config.show_hidden = !self.config.show_hidden;
            self.config.save();
            let h = self.config.show_hidden;
            self.left.reload_all(h);
            if let Some(r) = &mut self.right {
                r.reload_all(h);
            }
        }
        if kb_reload {
            let h = self.config.show_hidden;
            self.focused_pane_mut().active_mut().reload(h);
        }
        if kb_go_up {
            let h = self.config.show_hidden;
            self.focused_pane_mut().active_mut().go_back(h);
        }
        if let Some(idx) = kb_switch_tab {
            let n = self.focused_pane().tabs.len();
            if idx < n {
                self.focused_pane_mut().active_tab = idx;
            }
        }
        if let Some(direction) = kb_cycle_tab {
            let pane = self.focused_pane_mut();
            pane.active_tab = cycle_tab_index(pane.active_tab, pane.tabs.len(), direction);
        }
        if kb_switch_focus {
            self.focus = if self.focus == PaneSide::Left && self.right.is_some() {
                PaneSide::Right
            } else {
                PaneSide::Left
            };
        }
        // ── Git repo detection ────────────────────────────────────────────────
        let active_path = self.focused_pane().active().current_path.clone();
        if self.git_checked_path.as_ref() != Some(&active_path) {
            self.git_checked_path = Some(active_path.clone());
            self.git_workdir = git_repo::detect_repo(&active_path);
            if self.git_panel_open {
                if self.git_workdir.is_some() {
                    self.refresh_git();
                } else {
                    self.git_panel_open = false;
                }
            }
        }
        if kb_toggle_git {
            if self.git_workdir.is_some() {
                self.git_panel_open = !self.git_panel_open;
                if self.git_panel_open {
                    self.refresh_git();
                }
            }
        }

        if kb_toggle_terminal {
            self.toggle_terminal(ctx);
        }

        // ── Terminal CWD sync (terminal → browser) ────────────────────────────
        let maybe_new_cwd: Option<PathBuf> = self
            .terminals
            .get(self.terminal_active)
            .and_then(|t| t.grid.lock().ok())
            .and_then(|mut g| g.take_cwd_update());
        if let Some(new_cwd) = maybe_new_cwd {
            if new_cwd.exists() {
                let h = self.config.show_hidden;
                self.focused_pane_mut()
                    .active_mut()
                    .navigate(new_cwd.clone(), h);
                self.terminal_last_sync_path = Some(new_cwd);
            }
        }

        // ── Browser → terminal CWD sync ───────────────────────────────────────
        if self.terminal_open && !self.terminals.is_empty() {
            let current_path = self.focused_pane().active().current_path.clone();
            let last = self.terminal_last_sync_path.clone();
            if last.as_ref().map_or(false, |l| l != &current_path) {
                if let Some(term) = self.terminals.get_mut(self.terminal_active) {
                    let escaped = current_path.to_string_lossy().replace('\'', "'\\''");
                    term.write_input(format!("cd '{}'\r", escaped).as_bytes());
                }
                self.terminal_last_sync_path = Some(current_path);
            } else if last.is_none() {
                self.terminal_last_sync_path = Some(current_path);
            }
        }

        if kb_open_search && !self.search_open {
            let root = self.focused_pane().active().current_path.clone();
            let ctx2 = ctx.clone();
            self.search_query.clear();
            self.search_selected = 0;
            self.search_results.clear();
            self.search_engine = Some(SearchEngine::new(
                root,
                Arc::new(move || ctx2.request_repaint()),
            ));
            self.search_open = true;
            self.search_just_opened = true;
        }

        // ── Search overlay ────────────────────────────────────────────────────
        if self.search_open {
            if let Some(engine) = &mut self.search_engine {
                engine.set_query(&self.search_query);
                engine.tick();
                self.search_results = engine.results(200);
            }

            let root = self
                .search_engine
                .as_ref()
                .map(|e| e.root.clone())
                .unwrap_or_else(|| self.focused_pane().active().current_path.clone());
            let just_opened = self.search_just_opened;
            self.search_just_opened = false;

            match search_overlay::show(
                ctx,
                &root,
                &mut self.search_query,
                &self.search_results,
                &mut self.search_selected,
                just_opened,
            ) {
                Some(search_overlay::SearchAction::Close) => {
                    self.search_open = false;
                    self.search_engine = None;
                }
                Some(search_overlay::SearchAction::Navigate(path)) => {
                    let dir = if path.is_dir() {
                        path.clone()
                    } else {
                        path.parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or(path.clone())
                    };
                    let h = self.config.show_hidden;
                    self.focused_pane_mut()
                        .active_mut()
                        .navigate(dir.clone(), h);
                    // Pre-select the found file in the list
                    self.focused_pane_mut()
                        .active_mut()
                        .list_state
                        .select_only(path.clone());
                    self.config.last_path = Some(dir);
                    self.config.save();
                    self.toasts
                        .push(format!("Found: {}", display_text::file_name(&path)));
                    self.search_open = false;
                    self.search_engine = None;
                }
                None => {}
            }
        }

        // ── Preferences window ───────────────────────────────────────────────
        if self.prefs_open {
            let old_theme = self.config.theme;
            let old_custom_theme = self.config.custom_theme.clone();
            let old_hidden = self.config.show_hidden;
            let result = prefs::show(
                ctx,
                &mut self.prefs_open,
                &mut self.config,
                &self.custom_themes,
            );
            if result.config_changed {
                if self.config.custom_theme != old_custom_theme {
                    if let Some(ref id) = self.config.custom_theme {
                        if let Some(t) = self.custom_themes.iter().find(|t| t.id == *id) {
                            apply_custom_theme(ctx, &t.colors);
                        }
                    } else {
                        apply_theme(ctx, self.config.theme);
                    }
                } else if self.config.theme != old_theme {
                    apply_theme(ctx, self.config.theme);
                }
                if self.config.show_hidden != old_hidden {
                    let h = self.config.show_hidden;
                    self.left.reload_all(h);
                    if let Some(r) = &mut self.right {
                        r.reload_all(h);
                    }
                }
                self.config.save();
            }
        }

        // ── Toasts ────────────────────────────────────────────────────────────
        self.toasts.show(ctx);

        // ── Status bar ────────────────────────────────────────────────────────
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let count = self.focused_pane().active().entries.len();
                ui.label(
                    egui::RichText::new(format!(
                        "{} items{}",
                        count,
                        if self.config.show_hidden {
                            ""
                        } else {
                            " (hidden files excluded)"
                        }
                    ))
                    .small()
                    .weak(),
                );
            });
        });

        // ── Toolbar ───────────────────────────────────────────────────────────
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(4.0);
                let can_back = self.focused_pane().active().can_go_back();
                ui.add_enabled_ui(can_back, |ui| {
                    if ui
                        .button("◀")
                        .on_hover_text("Go back (Backspace)")
                        .clicked()
                    {
                        let h = self.config.show_hidden;
                        self.focused_pane_mut().active_mut().go_back(h);
                    }
                });
                if ui.button("↺").on_hover_text("Reload (⌘R)").clicked() {
                    let h = self.config.show_hidden;
                    self.focused_pane_mut().active_mut().reload(h);
                }
                ui.separator();
                let hidden_label = if self.config.show_hidden {
                    "Hide Hidden"
                } else {
                    "Show Hidden"
                };
                if ui
                    .button(hidden_label)
                    .on_hover_text("Toggle hidden files (⌘⇧.)")
                    .clicked()
                {
                    self.config.show_hidden = !self.config.show_hidden;
                    self.config.save();
                    let h = self.config.show_hidden;
                    self.left.reload_all(h);
                    if let Some(r) = &mut self.right {
                        r.reload_all(h);
                    }
                }
                ui.separator();
                if ui
                    .button("⊞ New Window")
                    .on_hover_text("Open new window (⌘N)")
                    .clicked()
                {
                    Self::open_new_window();
                }
                if self.right.is_some() {
                    ui.separator();
                    if ui
                        .button("✕ Close Split")
                        .on_hover_text("Close right pane (⌘\\)")
                        .clicked()
                    {
                        self.right = None;
                        self.focus = PaneSide::Left;
                        self.tab_drag = None;
                    }
                }
                if let Some(_wd) = &self.git_workdir {
                    ui.separator();
                    let git_label = if self.git_panel_open {
                        "Git ▾"
                    } else {
                        "Git ▸"
                    };
                    if ui
                        .button(git_label)
                        .on_hover_text("Toggle git panel (⌘G)")
                        .clicked()
                    {
                        self.git_panel_open = !self.git_panel_open;
                        if self.git_panel_open {
                            self.refresh_git();
                        }
                    }
                }
                ui.separator();
                let term_label = if self.terminal_open {
                    "Terminal ▾"
                } else {
                    "Terminal ▸"
                };
                if ui
                    .button(term_label)
                    .on_hover_text("Toggle terminal panel (⌘J)")
                    .clicked()
                {
                    self.toggle_terminal(ctx);
                }
                ui.separator();
                if ui.button("⚙").on_hover_text("Preferences (⌘,)").clicked() {
                    self.prefs_open = true;
                }
            });
        });

        // ── Git panel (bottom position only — right mode is rendered inside CentralPanel) ──
        if self.git_panel_open && !self.config.git_panel_right {
            if let Some(wd) = self.git_workdir.clone() {
                egui::TopBottomPanel::bottom("git_panel_bottom")
                    .exact_height(self.config.git_panel_height)
                    .resizable(false)
                    .show(ctx, |ui| {
                        let actions = git_panel::show(ui, &wd, &mut self.git_panel, false);
                        self.handle_git_actions(actions, &wd);
                    });
            }
        }

        // ── Terminal panel ────────────────────────────────────────────────────
        if self.terminal_open && !self.terminals.is_empty() {
            let fallback_cwd = self.focused_pane().active().current_path.clone();
            let terminal_app_pref = self.config.terminal;
            let panel_height = self.config.terminal_panel_height;
            let active = self.terminal_active;
            egui::TopBottomPanel::bottom("terminal_panel")
                .exact_height(panel_height)
                .resizable(false)
                .show(ctx, |ui| {
                    match terminal_panel::show(ui, &mut self.terminals, active) {
                        Some(TerminalPanelEvent::OpenInTerminal) => {
                            let cwd = self
                                .terminals
                                .get(active)
                                .and_then(|t| t.grid.lock().ok())
                                .and_then(|g| g.cwd.clone())
                                .unwrap_or_else(|| fallback_cwd.clone());
                            opener::open_in_terminal(&cwd, terminal_app_pref);
                        }
                        Some(TerminalPanelEvent::NewTab) => {
                            let cwd = self.focused_pane().active().current_path.clone();
                            let ctx2 = ctx.clone();
                            if let Ok(t) = TerminalState::spawn(
                                80,
                                24,
                                &cwd,
                                Arc::new(move || ctx2.request_repaint()),
                            ) {
                                self.terminals.push(t);
                                self.terminal_active = self.terminals.len() - 1;
                                self.terminal_last_sync_path = None;
                            }
                        }
                        Some(TerminalPanelEvent::CloseTab(idx)) => {
                            self.terminals.remove(idx);
                            if self.terminals.is_empty() {
                                self.terminal_open = false;
                            } else {
                                if self.terminal_active >= self.terminals.len() {
                                    self.terminal_active = self.terminals.len() - 1;
                                }
                                self.terminal_last_sync_path = None;
                            }
                        }
                        Some(TerminalPanelEvent::SwitchTab(idx)) => {
                            self.terminal_active = idx;
                            self.terminal_last_sync_path = None;
                        }
                        None => {}
                    }
                });
        }

        // ── Central panel — sidebar + manual split ───────────────────────────
        let file_dragging_paths = self.file_drag.as_ref().map(|drag| drag.paths.clone());
        let mut pending_internal_drop: Option<InternalFileDrop> = None;
        let mut rendered_left_rect: Option<egui::Rect> = None;
        let mut rendered_right_rect: Option<egui::Rect> = None;
        let current_path_for_sidebar = self.focused_pane().active().current_path.clone();
        let active_tag = self.focused_pane().active().tag_filter.clone();
        if self
            .clipboard_op
            .as_ref()
            .map_or(false, |op| op.change_count != clipboard::change_count())
        {
            self.clipboard_op = None;
        }
        let cut_paths: Vec<PathBuf> = self
            .clipboard_op
            .as_ref()
            .filter(|op| op.kind == ClipboardKind::Cut)
            .map_or_else(Vec::new, |op| op.paths.clone());
        let has_clipboard = self.clipboard_op.is_some() || clipboard::has_files();

        let is_tab_dragging = self.tab_drag.is_some();
        let mut git_right_actions: Vec<GitPanelAction> = Vec::new();

        egui::CentralPanel::default().show(ctx, |ui| {
            let total_rect = ui.available_rect_before_wrap();

            // ── Sidebar ───────────────────────────────────────────────────────
            let sb_w = self.config.sidebar_width;
            let sb_div_x = total_rect.min.x + sb_w;
            let sidebar_rect =
                egui::Rect::from_min_max(total_rect.min, egui::pos2(sb_div_x, total_rect.max.y));
            // Full pane area starts after the 1 px divider line.
            let git_right_w = if self.git_panel_open && self.config.git_panel_right {
                let avail = (total_rect.width() - sb_w - 1.0 - 200.0).max(200.0);
                self.config.git_panel_width.clamp(200.0, avail)
            } else {
                0.0
            };
            let full_rect = egui::Rect::from_min_max(
                egui::pos2(sb_div_x + 1.0, total_rect.min.y),
                egui::pos2(total_rect.max.x - git_right_w, total_rect.max.y),
            );
            self.content_rect = full_rect;

            // Paint sidebar background explicitly so no gaps appear on resize.
            ui.painter().rect_filled(
                sidebar_rect,
                egui::CornerRadius::ZERO,
                ui.visuals().panel_fill,
            );

            // Sidebar content.
            let mut sidebar_nav: Option<PathBuf> = None;
            let mut sidebar_bookmark: Option<PathBuf> = None;
            ui.allocate_new_ui(
                egui::UiBuilder::new()
                    .max_rect(sidebar_rect)
                    .id_salt("sidebar"),
                |ui| {
                ui.set_clip_rect(sidebar_rect);
                egui::ScrollArea::vertical()
                    .drag_to_scroll(false)
                    .show(ui, |ui| {
                        for action in sidebar::show(
                            ui,
                            &mut self.bookmarks,
                            &self.global_tags,
                            &current_path_for_sidebar,
                            file_dragging_paths.as_deref(),
                            active_tag.as_deref(),
                            &mut self.new_tag_input,
                            &mut self.new_tag_color,
                            &mut self.edit_tag_idx,
                            &mut self.edit_tag_name,
                            &mut self.edit_tag_color,
                        ) {
                            match action {
                                SidebarAction::Navigate(p) => sidebar_nav = Some(p),
                                SidebarAction::OpenFile(p) => opener::open_file(&p),
                                SidebarAction::AddBookmark(p) => sidebar_bookmark = Some(p),
                                SidebarAction::MoveFilesTo(paths, to_dir) => {
                                    if pending_internal_drop.is_none() {
                                        pending_internal_drop = Some(InternalFileDrop {
                                            paths,
                                            destination: to_dir,
                                        });
                                    }
                                }
                                SidebarAction::FilterTag(tag) => {
                                    let tab = self.focused_pane_mut().active_mut();
                                    let results = if let Some(ref name) = tag {
                                            let mut results =
                                                crate::core::global_tags::search_by_tag(name);
                                        // Spotlight may not have indexed recently-applied tags yet.
                                        // Merge in any matches from the currently loaded directory.
                                        for entry in &tab.entries {
                                            if entry.tags.iter().any(|t| &t.name == name)
                                                && !results.contains(&entry.path)
                                            {
                                                results.push(entry.path.clone());
                                            }
                                        }
                                            results
                                                .sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                                        Some(results)
                                    } else {
                                        None
                                    };
                                    tab.set_tag_view(tag, results);
                                }
                                SidebarAction::CreateTag(name, color) => {
                                    if !self.global_tags.add(name.clone(), color) {
                                            self.toasts
                                                .push(format!("Tag '{}' already exists", name));
                                    }
                                }
                                SidebarAction::DeleteTag(idx) => {
                                    self.global_tags.remove(idx);
                                    if self.edit_tag_idx == Some(idx) {
                                        self.edit_tag_idx = None;
                                    }
                                }
                                SidebarAction::StartEdit(idx) => {
                                    if let Some(tag) = self.global_tags.items.get(idx) {
                                        self.edit_tag_idx = Some(idx);
                                        self.edit_tag_name = tag.name.clone();
                                        self.edit_tag_color = tag.color;
                                    }
                                }
                                SidebarAction::CommitEdit(idx, name, color) => {
                                    self.global_tags.rename(idx, name);
                                    self.global_tags.set_color(idx, color);
                                    self.edit_tag_idx = None;
                                }
                                SidebarAction::CancelEdit => {
                                    self.edit_tag_idx = None;
                                }
                            }
                        }
                    });
                },
            );

            if let Some(p) = sidebar_nav {
                let h = self.config.show_hidden;
                self.focused_pane_mut().active_mut().navigate(p.clone(), h);
                self.config.last_path = Some(p);
                self.config.save();
            }
            if let Some(p) = sidebar_bookmark {
                let name = display_text::file_name(&p);
                self.bookmarks.add(p);
                self.toasts.push(format!("Bookmarked: {}", name));
            }

            // Sidebar divider — 1 px visual line + 7 px invisible hit area.
            let sb_div_color = if {
                let hit = egui::Rect::from_min_max(
                    egui::pos2(sb_div_x - 3.0, total_rect.top()),
                    egui::pos2(sb_div_x + 4.0, total_rect.bottom()),
                );
                let div_id = ui.id().with("sidebar_divider");
                let div_resp = ui.interact(hit, div_id, egui::Sense::drag());
                if div_resp.dragged() {
                    self.config.sidebar_width =
                        (self.config.sidebar_width + div_resp.drag_delta().x).clamp(120.0, 480.0);
                }
                if div_resp.drag_stopped() {
                    self.config.save();
                }
                if div_resp.hovered() || div_resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                div_resp.hovered() || div_resp.dragged()
            } {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            };
            ui.painter().vline(
                sb_div_x,
                total_rect.y_range(),
                egui::Stroke::new(1.0, sb_div_color),
            );

            // ── Terminal panel resize handle ──────────────────────────────────
            if self.terminal_open {
                let div_rect = egui::Rect::from_min_max(
                    egui::pos2(full_rect.left(), full_rect.bottom() - 3.0),
                    egui::pos2(full_rect.right(), full_rect.bottom() + 3.0),
                );
                let div_id = ui.id().with("terminal_panel_divider");
                let div_resp = ui.interact(div_rect, div_id, egui::Sense::drag());
                if div_resp.dragged() {
                    let dy = div_resp.drag_delta().y;
                    self.config.terminal_panel_height =
                        (self.config.terminal_panel_height - dy).clamp(80.0, 600.0);
                }
                if div_resp.drag_stopped() {
                    self.config.save();
                }
                if div_resp.hovered() || div_resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
            }

            // ── Git panel resize handle (bottom position only) ────────────────
            if self.git_panel_open && !self.config.git_panel_right && !self.terminal_open {
                let div_rect = egui::Rect::from_min_max(
                    egui::pos2(full_rect.left(), full_rect.bottom() - 3.0),
                    egui::pos2(full_rect.right(), full_rect.bottom() + 3.0),
                );
                let div_id = ui.id().with("git_panel_divider");
                let div_resp = ui.interact(div_rect, div_id, egui::Sense::drag());
                if div_resp.dragged() {
                    let dy = div_resp.drag_delta().y;
                    self.config.git_panel_height =
                        (self.config.git_panel_height - dy).clamp(80.0, 600.0);
                }
                if div_resp.drag_stopped() {
                    self.config.save();
                }
                if div_resp.hovered() || div_resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
            }

            if self.right.is_some() {
                // ── Divider ──────────────────────────────────────────────────
                let div_x = full_rect.left() + full_rect.width() * self.split_ratio;
                // Divider is 6 px wide centered on div_x; panes start 6 px outside it.
                let half: f32 = 3.0;
                let gap: f32 = 6.0;
                let left_rect = egui::Rect::from_min_max(
                    full_rect.min,
                    egui::pos2(div_x - half - gap, full_rect.max.y),
                );
                let div_rect = egui::Rect::from_min_max(
                    egui::pos2(div_x - half, full_rect.min.y),
                    egui::pos2(div_x + half, full_rect.max.y),
                );
                let right_rect = egui::Rect::from_min_max(
                    egui::pos2(div_x + half + gap, full_rect.min.y),
                    full_rect.max,
                );
                rendered_left_rect = Some(left_rect);
                rendered_right_rect = Some(right_rect);

                let div_id = ui.id().with("split_divider");
                let div_resp = ui.interact(div_rect, div_id, egui::Sense::drag());
                if div_resp.dragged() {
                    let dx = div_resp.drag_delta().x;
                    self.split_ratio =
                        (self.split_ratio + dx / full_rect.width()).clamp(0.15, 0.85);
                }
                if div_resp.hovered() || div_resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let div_color = if div_resp.hovered() || div_resp.dragged() {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                };
                ui.painter()
                    .rect_filled(div_rect, egui::CornerRadius::ZERO, div_color);

                // ── Left pane ─────────────────────────────────────────────────
                let left_focus = self.focus == PaneSide::Left;
                let left_drop_target = is_tab_dragging
                    && self
                        .tab_drag
                        .as_ref()
                        .map_or(false, |d| d.from == PaneSide::Right);

                let (left_tab_actions, left_file_actions, left_focus_clicked) = render_pane(
                    ui,
                    left_rect,
                    &mut self.left,
                    left_focus,
                    left_drop_target,
                    false,
                    "left",
                    &self.global_tags,
                    &cut_paths,
                    has_clipboard,
                    file_dragging_paths.as_deref(),
                );

                for action in left_tab_actions {
                    match action {
                        tab_bar::TabBarAction::Switch(i) => {
                            self.left.active_tab = i;
                            self.focus = PaneSide::Left;
                        }
                        tab_bar::TabBarAction::Close(i) => {
                            self.left.close_tab(i);
                        }
                        tab_bar::TabBarAction::New => {
                            let h = self.config.show_hidden;
                            self.left.new_tab(h);
                            self.focus = PaneSide::Left;
                        }
                        tab_bar::TabBarAction::DragTab(i) => {
                            self.tab_drag = Some(TabDrag {
                                from: PaneSide::Left,
                                tab_idx: i,
                            });
                        }
                    }
                }
                let left_ai = self.left.active_tab;
                let terminal = self.config.terminal;
                let (left_special, left_regular): (Vec<_>, Vec<_>) =
                    left_file_actions.into_iter().partition(|a| {
                        matches!(
                            a,
                            FileListAction::StartCreating(_)
                                | FileListAction::CreateItem(_, _)
                                | FileListAction::StartRename(_)
                                | FileListAction::RenameItem(_, _)
                                | FileListAction::CopyFiles(_)
                                | FileListAction::CutFiles(_)
                                | FileListAction::PasteHere
                        )
                    });
                let (left_creating, left_clipboard): (Vec<_>, Vec<_>) =
                    left_special.into_iter().partition(|a| {
                        matches!(
                            a,
                            FileListAction::StartCreating(_)
                                | FileListAction::CreateItem(_, _)
                                | FileListAction::StartRename(_)
                                | FileListAction::RenameItem(_, _)
                        )
                    });
                let mut left_outcome = Self::handle_file_actions(
                    left_regular,
                    &mut self.bookmarks,
                    &mut self.toasts,
                    terminal,
                );
                if let Some(paths) = left_outcome.drag_started.take() {
                    self.file_drag = Some(FileDragState {
                        paths,
                        origin_pane: PaneSide::Left,
                    });
                }
                if pending_internal_drop.is_none() {
                    pending_internal_drop = left_outcome.internal_drop.take();
                }
                let (left_create_changed_dirs, left_create_sel) = Self::handle_creating_actions(
                    left_creating,
                    &mut self.left.tabs[left_ai],
                    &mut self.toasts,
                );
                let left_paste_dir = self.left.tabs[left_ai].current_path.clone();
                let left_clip = Self::handle_clipboard_actions(
                    left_clipboard,
                    &mut self.clipboard_op,
                    &left_paste_dir,
                    &mut self.toasts,
                );
                for dir in left_create_changed_dirs {
                    push_unique_path(&mut left_outcome.changed_dirs, dir);
                }
                self.reload_tabs_in_dirs(&left_outcome.changed_dirs);
                if let Some(p) = left_create_sel {
                    self.left.tabs[left_ai].list_state.select_only(p);
                }
                self.reload_after_paste(&left_clip, &left_paste_dir);
                let left_created: Vec<PathBuf> = left_clip
                    .created
                    .iter()
                    .filter(|path| path.exists())
                    .cloned()
                    .collect();
                if !left_created.is_empty() {
                    self.left.tabs[left_ai]
                        .list_state
                        .select_all(left_created.iter());
                }
                if let Some(p) = left_outcome.navigate_to {
                    let h = self.config.show_hidden;
                    self.left.tabs[self.left.active_tab].navigate(p.clone(), h);
                    if let Some(sel) = left_outcome.select_after_nav {
                        self.left.tabs[left_ai].list_state.select_only(sel);
                    }
                    self.config.last_path = Some(p);
                    self.config.save();
                }
                if left_outcome.clear_tag_filter {
                    self.left.tabs[left_ai].set_tag_view(None, None);
                }
                if let Some(p) = left_outcome.quicklook_path {
                    self.do_quicklook(p);
                }
                if left_focus_clicked {
                    self.focus = PaneSide::Left;
                }

                // ── Right pane ────────────────────────────────────────────────
                let right_focus = self.focus == PaneSide::Right;
                let right_drop_target = is_tab_dragging
                    && self
                        .tab_drag
                        .as_ref()
                        .map_or(false, |d| d.from == PaneSide::Left);

                // Collect into locals so right borrow ends before handle_file_actions
                let (right_tab_actions, right_file_actions, right_focus_clicked) = {
                    let right = self.right.as_mut().unwrap();
                    render_pane(
                        ui,
                        right_rect,
                        right,
                        right_focus,
                        right_drop_target,
                        false,
                        "right",
                        &self.global_tags,
                        &cut_paths,
                        has_clipboard,
                        file_dragging_paths.as_deref(),
                    )
                };

                let mut remove_right = false;
                for action in right_tab_actions {
                    match action {
                        tab_bar::TabBarAction::Switch(i) => {
                            self.right.as_mut().unwrap().active_tab = i;
                            self.focus = PaneSide::Right;
                        }
                        tab_bar::TabBarAction::Close(i) => {
                            if self.right.as_mut().unwrap().close_tab(i) {
                                remove_right = true;
                            }
                        }
                        tab_bar::TabBarAction::New => {
                            let h = self.config.show_hidden;
                            self.right.as_mut().unwrap().new_tab(h);
                            self.focus = PaneSide::Right;
                        }
                        tab_bar::TabBarAction::DragTab(i) => {
                            self.tab_drag = Some(TabDrag {
                                from: PaneSide::Right,
                                tab_idx: i,
                            });
                        }
                    }
                }
                let right_ai = self.right.as_ref().map(|r| r.active_tab).unwrap_or(0);
                let terminal = self.config.terminal;
                let (right_special, right_regular): (Vec<_>, Vec<_>) =
                    right_file_actions.into_iter().partition(|a| {
                        matches!(
                            a,
                            FileListAction::StartCreating(_)
                                | FileListAction::CreateItem(_, _)
                                | FileListAction::StartRename(_)
                                | FileListAction::RenameItem(_, _)
                                | FileListAction::CopyFiles(_)
                                | FileListAction::CutFiles(_)
                                | FileListAction::PasteHere
                        )
                    });
                let (right_creating, right_clipboard): (Vec<_>, Vec<_>) =
                    right_special.into_iter().partition(|a| {
                        matches!(
                            a,
                            FileListAction::StartCreating(_)
                                | FileListAction::CreateItem(_, _)
                                | FileListAction::StartRename(_)
                                | FileListAction::RenameItem(_, _)
                        )
                    });
                let mut right_outcome = Self::handle_file_actions(
                    right_regular,
                    &mut self.bookmarks,
                    &mut self.toasts,
                    terminal,
                );
                if let Some(paths) = right_outcome.drag_started.take() {
                    self.file_drag = Some(FileDragState {
                        paths,
                        origin_pane: PaneSide::Right,
                    });
                }
                if pending_internal_drop.is_none() {
                    pending_internal_drop = right_outcome.internal_drop.take();
                }
                let (right_create_changed_dirs, right_create_sel) = Self::handle_creating_actions(
                    right_creating,
                    self.right.as_mut().unwrap().tabs.get_mut(right_ai).unwrap(),
                    &mut self.toasts,
                );
                let right_paste_dir = self
                    .right
                    .as_ref()
                    .map(|r| r.tabs[right_ai].current_path.clone())
                    .unwrap_or_default();
                let right_clip = Self::handle_clipboard_actions(
                    right_clipboard,
                    &mut self.clipboard_op,
                    &right_paste_dir,
                    &mut self.toasts,
                );
                for dir in right_create_changed_dirs {
                    push_unique_path(&mut right_outcome.changed_dirs, dir);
                }
                self.reload_tabs_in_dirs(&right_outcome.changed_dirs);
                if let (Some(r), Some(p)) = (&mut self.right, right_create_sel) {
                    r.tabs[right_ai].list_state.select_only(p);
                }
                self.reload_after_paste(&right_clip, &right_paste_dir);
                let right_created: Vec<PathBuf> = right_clip
                    .created
                    .iter()
                    .filter(|path| path.exists())
                    .cloned()
                    .collect();
                if !right_created.is_empty() {
                    if let Some(r) = &mut self.right {
                        r.tabs[right_ai].list_state.select_all(right_created.iter());
                    }
                }
                if let Some(p) = right_outcome.navigate_to {
                    let h = self.config.show_hidden;
                    if let Some(r) = &mut self.right {
                        r.tabs[right_ai].navigate(p, h);
                        if let Some(sel) = right_outcome.select_after_nav {
                            r.tabs[right_ai].list_state.select_only(sel);
                        }
                    }
                }
                if right_outcome.clear_tag_filter {
                    if let Some(r) = &mut self.right {
                        r.tabs[right_ai].set_tag_view(None, None);
                    }
                }
                if let Some(p) = right_outcome.quicklook_path {
                    self.do_quicklook(p);
                }
                if right_focus_clicked {
                    self.focus = PaneSide::Right;
                }
                if remove_right {
                    self.right = None;
                    self.focus = PaneSide::Left;
                }
            } else {
                // ── Only left pane (full rect) ────────────────────────────────
                rendered_left_rect = Some(full_rect);
                rendered_right_rect = None;
                let left_drop_target = is_tab_dragging; // can only drag from left, so never true

                // Draw drop zone overlay on right half when dragging a tab
                if is_tab_dragging {
                    let drop_rect = egui::Rect::from_min_max(
                        egui::pos2(full_rect.center().x, full_rect.top()),
                        full_rect.max,
                    );
                    ui.painter().rect_filled(
                        drop_rect,
                        egui::CornerRadius::same(4),
                        egui::Color32::from_rgba_unmultiplied(100, 160, 255, 25),
                    );
                    ui.painter().rect_stroke(
                        drop_rect,
                        egui::CornerRadius::same(4),
                        egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        drop_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Drop here to split",
                        egui::TextStyle::Body.resolve(ui.style()),
                        ui.visuals().selection.bg_fill,
                    );
                }

                let _ = left_drop_target;
                let (left_tab_actions, left_file_actions, _) = render_pane(
                    ui,
                    full_rect,
                    &mut self.left,
                    true,
                    false,
                    true,
                    "left",
                    &self.global_tags,
                    &cut_paths,
                    has_clipboard,
                    file_dragging_paths.as_deref(),
                );

                for action in left_tab_actions {
                    match action {
                        tab_bar::TabBarAction::Switch(i) => {
                            self.left.active_tab = i;
                        }
                        tab_bar::TabBarAction::Close(i) => {
                            self.left.close_tab(i);
                        }
                        tab_bar::TabBarAction::New => {
                            let h = self.config.show_hidden;
                            self.left.new_tab(h);
                        }
                        tab_bar::TabBarAction::DragTab(i) => {
                            self.tab_drag = Some(TabDrag {
                                from: PaneSide::Left,
                                tab_idx: i,
                            });
                        }
                    }
                }
                let left_ai = self.left.active_tab;
                let terminal = self.config.terminal;
                let (left_special, left_regular): (Vec<_>, Vec<_>) =
                    left_file_actions.into_iter().partition(|a| {
                        matches!(
                            a,
                            FileListAction::StartCreating(_)
                                | FileListAction::CreateItem(_, _)
                                | FileListAction::StartRename(_)
                                | FileListAction::RenameItem(_, _)
                                | FileListAction::CopyFiles(_)
                                | FileListAction::CutFiles(_)
                                | FileListAction::PasteHere
                        )
                    });
                let (left_creating, left_clipboard): (Vec<_>, Vec<_>) =
                    left_special.into_iter().partition(|a| {
                        matches!(
                            a,
                            FileListAction::StartCreating(_)
                                | FileListAction::CreateItem(_, _)
                                | FileListAction::StartRename(_)
                                | FileListAction::RenameItem(_, _)
                        )
                    });
                let mut left_outcome = Self::handle_file_actions(
                    left_regular,
                    &mut self.bookmarks,
                    &mut self.toasts,
                    terminal,
                );
                if let Some(paths) = left_outcome.drag_started.take() {
                    self.file_drag = Some(FileDragState {
                        paths,
                        origin_pane: PaneSide::Left,
                    });
                }
                if pending_internal_drop.is_none() {
                    pending_internal_drop = left_outcome.internal_drop.take();
                }
                let (left_create_changed_dirs, left_create_sel) = Self::handle_creating_actions(
                    left_creating,
                    &mut self.left.tabs[left_ai],
                    &mut self.toasts,
                );
                let left_paste_dir = self.left.tabs[left_ai].current_path.clone();
                let left_clip = Self::handle_clipboard_actions(
                    left_clipboard,
                    &mut self.clipboard_op,
                    &left_paste_dir,
                    &mut self.toasts,
                );
                for dir in left_create_changed_dirs {
                    push_unique_path(&mut left_outcome.changed_dirs, dir);
                }
                self.reload_tabs_in_dirs(&left_outcome.changed_dirs);
                if let Some(p) = left_create_sel {
                    self.left.tabs[left_ai].list_state.select_only(p);
                }
                self.reload_after_paste(&left_clip, &left_paste_dir);
                let left_created: Vec<PathBuf> = left_clip
                    .created
                    .iter()
                    .filter(|path| path.exists())
                    .cloned()
                    .collect();
                if !left_created.is_empty() {
                    self.left.tabs[left_ai]
                        .list_state
                        .select_all(left_created.iter());
                }
                if let Some(p) = left_outcome.navigate_to {
                    let h = self.config.show_hidden;
                    self.left.tabs[left_ai].navigate(p.clone(), h);
                    if let Some(sel) = left_outcome.select_after_nav {
                        self.left.tabs[left_ai].list_state.select_only(sel);
                    }
                    self.config.last_path = Some(p);
                    self.config.save();
                }
                if left_outcome.clear_tag_filter {
                    self.left.tabs[left_ai].set_tag_view(None, None);
                }
                if let Some(p) = left_outcome.quicklook_path {
                    self.do_quicklook(p);
                }
            }

            // ── Git panel (right position, carved out of CentralPanel) ─────────
            if self.git_panel_open && self.config.git_panel_right {
                if let Some(ref wd) = self.git_workdir.clone() {
                    let gpr = egui::Rect::from_min_max(
                        egui::pos2(full_rect.max.x + 1.0, total_rect.min.y),
                        total_rect.max,
                    );
                    ui.painter().rect_filled(
                        gpr,
                        egui::CornerRadius::ZERO,
                        ui.visuals().panel_fill,
                    );
                    ui.painter().vline(
                        gpr.min.x - 1.0,
                        gpr.y_range(),
                        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                    );
                    // Drag divider
                    let div_hit = egui::Rect::from_min_max(
                        egui::pos2(gpr.min.x - 4.0, gpr.top()),
                        egui::pos2(gpr.min.x + 3.0, gpr.bottom()),
                    );
                    let div_id = ui.id().with("git_right_div");
                    let div_resp = ui.interact(div_hit, div_id, egui::Sense::drag());
                    if div_resp.dragged() {
                        self.config.git_panel_width = (self.config.git_panel_width
                            - div_resp.drag_delta().x)
                            .clamp(200.0, 700.0);
                    }
                    if div_resp.drag_stopped() {
                        self.config.save();
                    }
                    if div_resp.hovered() || div_resp.dragged() {
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    ui.allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(gpr)
                            .id_salt("git_panel_right"),
                        |ui| {
                            ui.set_clip_rect(gpr);
                            git_right_actions = git_panel::show(ui, wd, &mut self.git_panel, true);
                        },
                    );
                }
            }
        });

        // Process any actions emitted by the right-position git panel.
        if !git_right_actions.is_empty() {
            if let Some(wd) = self.git_workdir.clone() {
                self.handle_git_actions(git_right_actions, &wd);
            }
        }

        // ── Resolve one internal drop ─────────────────────────────────────────
        let hovered_pane = if pointer_released {
            ctx.pointer_hover_pos().and_then(|position| {
                if rendered_left_rect.is_some_and(|rect| rect.contains(position)) {
                    Some(PaneSide::Left)
                } else if rendered_right_rect.is_some_and(|rect| rect.contains(position)) {
                    Some(PaneSide::Right)
                } else {
                    None
                }
            })
        } else {
            None
        };
        let left_directory = self.left.active().current_path.clone();
        let right_directory = self
            .right
            .as_ref()
            .map(|right| right.active().current_path.clone());
        let internal_drop = resolve_internal_file_drop(
            pending_internal_drop,
            self.file_drag.as_ref(),
            hovered_pane,
            &left_directory,
            right_directory.as_ref(),
        );
        if let Some(drop) = internal_drop {
            // Clear before moving so this release cannot also begin a native drag.
            self.file_drag.take();
            self.perform_internal_file_drop(drop);
        }

        // ── External drag: trigger when cursor leaves the window ──────────────
        #[cfg(target_os = "macos")]
        if self.file_drag.is_some() {
            let window_rect = ctx.screen_rect();
            let cursor_left = ctx.input(|i| {
                i.pointer
                    .hover_pos()
                    .map_or(false, |p| !window_rect.contains(p))
            });
            if cursor_left {
                let drag = self.file_drag.take().unwrap();
                let path_refs: Vec<&std::path::Path> =
                    drag.paths.iter().map(PathBuf::as_path).collect();
                let gesture_id = self.next_external_drag_gesture_id;
                self.next_external_drag_gesture_id = gesture_id.wrapping_add(1).max(1);
                match crate::platform::drag::begin_external_drag(&path_refs, gesture_id) {
                    Ok(_) => {}
                    Err(error) => self
                        .toasts
                        .push(format!("Could not start external drag: {}", error)),
                }
            }
        }

        // ── Drop from Finder into the focused pane ────────────────────────────
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            let dest_dir = self.focused_pane().active().current_path.clone();
            let sources: Vec<PathBuf> = dropped
                .iter()
                .filter_map(|file| file.path.clone())
                .collect();
            let outcome = paste_paths(&sources, ClipboardKind::Copy, &dest_dir);
            for (path, error) in &outcome.failed {
                self.toasts.push(format!(
                    "Drop failed for {}: {}",
                    display_text::path(path),
                    error
                ));
            }
            // A recursive failure may still leave a partial destination. Always
            // reload the destination tabs so the UI reflects the actual disk state.
            let mut reload_dirs = vec![dest_dir.clone()];
            reload_dirs.extend(paste_reload_dirs(&outcome, &dest_dir));
            reload_dirs.dedup();
            self.reload_tabs_in_dirs(&reload_dirs);
            self.select_paste_results(&outcome.created, &dest_dir);
        }

        // ── Clear file drag state ─────────────────────────────────────────────
        if pointer_released {
            crate::platform::drag::cancel_pending_external_drag();
            self.file_drag.take();
        }

        self.process_session_persistence(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_session_now();
    }
}

/// Render one pane (tab bar + file list) into a rect within the given `ui`.
/// Returns (tab_bar_actions, file_list_actions, focus_clicked).
const PANE_SECTION_SEPARATOR_HEIGHT: f32 = 1.0;

fn pane_section_rects(content_rect: egui::Rect) -> (egui::Rect, egui::Rect) {
    let tab_bottom = (content_rect.top() + tab_bar::strip_height()).min(content_rect.bottom());
    let file_top = (tab_bottom + PANE_SECTION_SEPARATOR_HEIGHT).min(content_rect.bottom());
    let tab_rect = egui::Rect::from_min_max(
        content_rect.min,
        egui::pos2(content_rect.right(), tab_bottom),
    );
    let file_rect = egui::Rect::from_min_max(
        egui::pos2(content_rect.left(), file_top),
        content_rect.max,
    );
    (tab_rect, file_rect)
}

fn render_pane(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    pane: &mut PaneState,
    is_focused: bool,
    is_drop_target: bool,
    is_only_pane: bool,
    pane_id: &str,
    global_tags: &crate::core::global_tags::GlobalTags,
    cut_paths: &[PathBuf],
    has_clipboard: bool,
    dragging_paths: Option<&[PathBuf]>,
) -> (Vec<tab_bar::TabBarAction>, Vec<FileListAction>, bool) {
    let mut tab_actions = Vec::new();
    let mut file_actions = Vec::new();

    // Draw 3 px accent bar using the parent painter BEFORE entering the child UI,
    // so it is never clipped by the child's narrower max_rect.
    if is_focused && !is_only_pane {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            egui::CornerRadius::ZERO,
            ui.visuals().selection.bg_fill,
        );
    }

    // Inset content 3 px from the left so the bar doesn't overlap tab labels / breadcrumbs.
    let bar_w = if is_focused && !is_only_pane {
        3.0_f32
    } else {
        0.0_f32
    };
    let content_rect =
        egui::Rect::from_min_max(egui::pos2(rect.min.x + bar_w, rect.min.y), rect.max);
    let (tab_rect, file_rect) = pane_section_rects(content_rect);

    ui.allocate_new_ui(
        egui::UiBuilder::new()
            .max_rect(tab_rect)
            .id_salt((pane_id, "tabs")),
        |ui| {
            ui.set_clip_rect(tab_rect);
            let names = pane.tab_names();
            let (tb_actions, _) =
                tab_bar::show(ui, &names, pane.active_tab, is_drop_target);
            tab_actions = tb_actions;
        },
    );

    if file_rect.top() > tab_rect.bottom() {
        ui.painter().hline(
            content_rect.x_range(),
            tab_rect.bottom(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    }

    if file_rect.is_positive() {
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(file_rect)
                .id_salt((pane_id, "files")),
            |ui| {
                ui.set_clip_rect(file_rect);
                let ai = pane.active_tab;
                {
                    let t = &mut pane.tabs[ai];
                    sort_entries(
                        &mut t.entries,
                        t.list_state.sort_col,
                        t.list_state.sort_order,
                    );
                }
                let actions = {
                    let t = &mut pane.tabs[ai];
                    file_list::show(
                        ui,
                        &t.entries,
                        &mut t.list_state,
                        &t.current_path,
                        t.tag_filter.as_deref(),
                        global_tags,
                        cut_paths,
                        has_clipboard,
                        dragging_paths,
                        t.tag_search_results.as_deref(),
                    )
                };
                file_actions = actions;
            },
        );
    }

    // Detect click to focus without creating an overlapping interactive widget
    // (which would compete with file-list row interactions and swallow clicks).
    let focus_clicked = ui.input(|i| {
        i.pointer.any_pressed()
            && i.pointer
                .press_origin()
                .map_or(false, |pos| rect.contains(pos))
    });

    (tab_actions, file_actions, focus_clicked)
}

#[cfg(test)]
mod pane_layout_tests {
    use super::{pane_section_rects, PANE_SECTION_SEPARATOR_HEIGHT};
    use crate::ui::tab_bar;
    use eframe::egui;

    #[test]
    fn tab_strip_and_file_content_have_independent_fixed_rectangles() {
        let pane = egui::Rect::from_min_size(
            egui::pos2(12.0, 24.0),
            egui::vec2(360.0, 500.0),
        );
        let (tabs, files) = pane_section_rects(pane);

        assert_eq!(tabs.width(), pane.width());
        assert_eq!(tabs.height(), tab_bar::strip_height());
        assert_eq!(files.width(), pane.width());
        assert_eq!(files.bottom(), pane.bottom());
        assert_eq!(
            files.top(),
            tabs.bottom() + PANE_SECTION_SEPARATOR_HEIGHT
        );
    }

    #[test]
    fn a_short_pane_never_produces_negative_content_geometry() {
        let pane = egui::Rect::from_min_size(
            egui::pos2(5.0, 8.0),
            egui::vec2(100.0, 10.0),
        );
        let (tabs, files) = pane_section_rects(pane);

        assert!(tabs.is_positive());
        assert_eq!(tabs.bottom(), pane.bottom());
        assert_eq!(files.height(), 0.0);
        assert_eq!(files.width(), pane.width());
    }
}

fn apply_theme(ctx: &egui::Context, theme: crate::core::config::Theme) {
    use crate::core::config::Theme;
    let visuals = match theme {
        Theme::Dark => egui::Visuals::dark(),
        Theme::Light => egui::Visuals::light(),
        Theme::System => {
            // egui doesn't have native OS theme detection; default to dark
            egui::Visuals::dark()
        }
    };
    ctx.set_visuals(visuals);
}

fn blend_color(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (a.r() as f32 + (b.r() as f32 - a.r() as f32) * t) as u8,
        (a.g() as f32 + (b.g() as f32 - a.g() as f32) * t) as u8,
        (a.b() as f32 + (b.b() as f32 - a.b() as f32) * t) as u8,
    )
}

fn apply_custom_theme(ctx: &egui::Context, colors: &crate::core::themes::ThemeColors) {
    use egui::{Color32, Stroke};

    let bg = Color32::from_rgb(
        colors.background.r,
        colors.background.g,
        colors.background.b,
    );
    let text = Color32::from_rgb(
        colors.primary_text.r,
        colors.primary_text.g,
        colors.primary_text.b,
    );
    let subtle = Color32::from_rgb(
        colors.secondary_text.r,
        colors.secondary_text.g,
        colors.secondary_text.b,
    );
    let accent = Color32::from_rgb(colors.accent.r,         colors.accent.g,         colors.accent.b);

    let lum =
        (colors.background.r as u32 + colors.background.g as u32 + colors.background.b as u32) / 3;
    let mut v = if lum > 127 {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    v.panel_fill        = bg;
    v.window_fill       = bg;
    v.extreme_bg_color  = blend_color(bg, text, 0.05);
    v.faint_bg_color    = blend_color(bg, text, 0.03);
    v.code_bg_color     = blend_color(bg, text, 0.07);
    v.override_text_color = Some(text);
    v.hyperlink_color   = accent;

    v.selection.bg_fill = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 90);
    v.selection.stroke  = Stroke::new(1.0, accent);

    let w_bg        = blend_color(bg, text, 0.08);
    let w_bg_hover  = blend_color(bg, text, 0.15);
    let w_bg_active = blend_color(bg, text, 0.22);
    let border      = blend_color(bg, text, 0.25);
    let border_acc  = blend_color(border, accent, 0.3);

    v.widgets.noninteractive.bg_fill      = bg;
    v.widgets.noninteractive.weak_bg_fill = blend_color(bg, text, 0.04);
    v.widgets.noninteractive.bg_stroke    = Stroke::new(1.0, border);
    v.widgets.noninteractive.fg_stroke    = Stroke::new(1.0, subtle);

    v.widgets.inactive.bg_fill      = w_bg;
    v.widgets.inactive.weak_bg_fill = w_bg;
    v.widgets.inactive.bg_stroke    = Stroke::new(0.5, border);
    v.widgets.inactive.fg_stroke    = Stroke::new(1.0, text);

    v.widgets.hovered.bg_fill      = w_bg_hover;
    v.widgets.hovered.weak_bg_fill = w_bg_hover;
    v.widgets.hovered.bg_stroke    = Stroke::new(1.0, border_acc);
    v.widgets.hovered.fg_stroke    = Stroke::new(1.5, text);

    v.widgets.active.bg_fill      = w_bg_active;
    v.widgets.active.weak_bg_fill = w_bg_active;
    v.widgets.active.bg_stroke    = Stroke::new(1.0, accent);
    v.widgets.active.fg_stroke    = Stroke::new(2.0, text);

    v.widgets.open.bg_fill      = w_bg_hover;
    v.widgets.open.weak_bg_fill = w_bg_hover;
    v.widgets.open.bg_stroke    = Stroke::new(1.0, border);
    v.widgets.open.fg_stroke    = Stroke::new(1.5, text);

    ctx.set_visuals(v);
}

#[cfg(test)]
mod clipboard_tests {
    use super::{
        browser_session_snapshot, classify_external_drag_operation, external_drag_source_dirs,
        cycle_tab_index, file_list_owns_keyboard_commands, internal_drop_reload_dirs,
        move_error_message, move_path, paste_paths, paste_reload_dirs, push_unique_path,
        resolve_internal_file_drop, restore_browser_session, tab_cycle_key_event,
        take_tab_cycle_shortcut, update_pending_session, ClipboardKind, ExternalDragResult,
        FileDragState, InternalFileDrop, PaneSide, PaneState, TabCycleDirection, TabState,
        SESSION_SAVE_DEBOUNCE,
    };
    use crate::core::session::{
        PaneSession, SavedPaneSide, SessionState, TabSession, SESSION_VERSION,
    };
    use std::path::PathBuf;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "file-explorer-{}-{}-{}",
                name,
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

    #[test]
    fn copies_multiple_files_without_overwriting() {
        let root = TestDir::new("multi-copy");
        let source = root.0.join("source");
        let destination = root.0.join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let first = source.join("first.txt");
        let second = source.join("second.txt");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, "second").unwrap();

        let outcome = paste_paths(&[first, second], ClipboardKind::Copy, &destination);

        assert_eq!(outcome.succeeded_sources.len(), 2);
        assert!(outcome.failed.is_empty());
        assert_eq!(
            std::fs::read_to_string(destination.join("first.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("second.txt")).unwrap(),
            "second"
        );
    }

    #[test]
    fn collision_is_reported_and_existing_file_is_unchanged() {
        let root = TestDir::new("collision");
        let source = root.0.join("source");
        let destination = root.0.join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let incoming = source.join("same.txt");
        let existing = destination.join("same.txt");
        std::fs::write(&incoming, "incoming").unwrap();
        std::fs::write(&existing, "existing").unwrap();

        let outcome = paste_paths(&[incoming], ClipboardKind::Copy, &destination);

        assert!(outcome.succeeded_sources.is_empty());
        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(std::fs::read_to_string(existing).unwrap(), "existing");
    }

    #[test]
    fn cut_keeps_failed_source_and_moves_other_items() {
        let root = TestDir::new("partial-cut");
        let source = root.0.join("source");
        let destination = root.0.join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let blocked = source.join("blocked.txt");
        let movable = source.join("movable.txt");
        std::fs::write(&blocked, "blocked source").unwrap();
        std::fs::write(&movable, "movable").unwrap();
        std::fs::write(destination.join("blocked.txt"), "existing").unwrap();

        let outcome = paste_paths(
            &[blocked.clone(), movable.clone()],
            ClipboardKind::Cut,
            &destination,
        );

        assert_eq!(outcome.failed.len(), 1);
        assert_eq!(outcome.succeeded_sources, vec![movable.clone()]);
        assert!(blocked.exists());
        assert!(!movable.exists());
        assert!(destination.join("movable.txt").exists());
        assert_eq!(
            paste_reload_dirs(&outcome, &destination),
            vec![destination, source],
        );
    }

    #[test]
    fn moving_an_item_to_its_current_folder_is_prohibited() {
        let root = TestDir::new("same-folder-move");
        let source = root.0.join("LONG_NAME_AND_KOREAN_FIX_PLAN.md");
        std::fs::write(&source, "plan").unwrap();

        let error = move_path(&source, &root.0).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(move_error_message(&source, &error).starts_with("Move prohibited"));
        assert!(source.exists());
        assert_eq!(std::fs::read_to_string(source).unwrap(), "plan");
    }

    #[test]
    fn internal_move_rejects_an_existing_destination() {
        let root = TestDir::new("move-collision");
        let source_dir = root.0.join("source");
        let destination_dir = root.0.join("destination");
        std::fs::create_dir(&source_dir).unwrap();
        std::fs::create_dir(&destination_dir).unwrap();
        let source = source_dir.join("same.txt");
        let destination = destination_dir.join("same.txt");
        std::fs::write(&source, "source").unwrap();
        std::fs::write(&destination, "destination").unwrap();

        let error = move_path(&source, &destination_dir).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(source).unwrap(), "source");
        assert_eq!(std::fs::read_to_string(destination).unwrap(), "destination");
    }

    #[test]
    fn folder_cannot_be_moved_into_its_own_descendant() {
        let root = TestDir::new("descendant-move");
        let source = root.0.join("folder");
        let child = source.join("child");
        std::fs::create_dir_all(&child).unwrap();

        let error = move_path(&source, &child).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(source.exists());
        assert!(child.exists());
    }

    #[test]
    fn external_drag_operations_are_interpreted_exactly() {
        assert_eq!(
            classify_external_drag_operation(0),
            ExternalDragResult::Cancelled
        );
        assert_eq!(
            classify_external_drag_operation(1),
            ExternalDragResult::Copy
        );
        assert_eq!(
            classify_external_drag_operation(16),
            ExternalDragResult::Move
        );
        assert_eq!(
            classify_external_drag_operation(17),
            ExternalDragResult::Other(17)
        );
        assert_eq!(
            classify_external_drag_operation(4),
            ExternalDragResult::Other(4)
        );
    }

    #[test]
    fn external_drag_source_directories_are_deduplicated() {
        let paths = vec![
            PathBuf::from("/one/a.txt"),
            PathBuf::from("/one/b.txt"),
            PathBuf::from("/two/c.txt"),
        ];
        assert_eq!(
            external_drag_source_dirs(&paths),
            vec![PathBuf::from("/one"), PathBuf::from("/two")],
        );
    }

    fn left_file_drag(paths: Vec<PathBuf>) -> FileDragState {
        FileDragState {
            paths,
            origin_pane: PaneSide::Left,
        }
    }

    #[test]
    fn hovered_folder_wins_over_opposite_pane_root() {
        let drag = left_file_drag(vec![PathBuf::from("/left/item.txt")]);
        let folder_drop = InternalFileDrop {
            paths: drag.paths.clone(),
            destination: PathBuf::from("/right/exact-folder"),
        };

        let resolved = resolve_internal_file_drop(
            Some(folder_drop.clone()),
            Some(&drag),
            Some(PaneSide::Right),
            &PathBuf::from("/left"),
            Some(&PathBuf::from("/right")),
        );

        assert_eq!(resolved, Some(folder_drop));
    }

    #[test]
    fn sidebar_target_wins_over_pane_root_fallback() {
        let drag = left_file_drag(vec![PathBuf::from("/left/item.txt")]);
        let sidebar_drop = InternalFileDrop {
            paths: drag.paths.clone(),
            destination: PathBuf::from("/bookmarks/project"),
        };

        let resolved = resolve_internal_file_drop(
            Some(sidebar_drop.clone()),
            Some(&drag),
            Some(PaneSide::Right),
            &PathBuf::from("/left"),
            Some(&PathBuf::from("/right")),
        );

        assert_eq!(resolved, Some(sidebar_drop));
    }

    #[test]
    fn opposite_pane_empty_space_uses_its_active_directory() {
        let drag = left_file_drag(vec![PathBuf::from("/left/item.txt")]);

        let resolved = resolve_internal_file_drop(
            None,
            Some(&drag),
            Some(PaneSide::Right),
            &PathBuf::from("/left"),
            Some(&PathBuf::from("/right/active-tab")),
        );

        assert_eq!(
            resolved,
            Some(InternalFileDrop {
                paths: drag.paths,
                destination: PathBuf::from("/right/active-tab"),
            })
        );
    }

    #[test]
    fn opposite_pane_fallback_also_works_right_to_left() {
        let drag = FileDragState {
            paths: vec![PathBuf::from("/right/item.txt")],
            origin_pane: PaneSide::Right,
        };

        let resolved = resolve_internal_file_drop(
            None,
            Some(&drag),
            Some(PaneSide::Left),
            &PathBuf::from("/left/active-tab"),
            Some(&PathBuf::from("/right")),
        );

        assert_eq!(
            resolved,
            Some(InternalFileDrop {
                paths: drag.paths,
                destination: PathBuf::from("/left/active-tab"),
            })
        );
    }

    #[test]
    fn origin_pane_empty_space_does_not_move_files() {
        let drag = left_file_drag(vec![PathBuf::from("/left/item.txt")]);

        let resolved = resolve_internal_file_drop(
            None,
            Some(&drag),
            Some(PaneSide::Left),
            &PathBuf::from("/left"),
            Some(&PathBuf::from("/right")),
        );

        assert_eq!(resolved, None);
    }

    #[test]
    fn app_level_drag_uses_the_destination_tab_active_at_release() {
        let drag = left_file_drag(vec![PathBuf::from("/left/item.txt")]);
        let newly_active_destination = PathBuf::from("/right/second-tab");

        let resolved = resolve_internal_file_drop(
            None,
            Some(&drag),
            Some(PaneSide::Right),
            &PathBuf::from("/left"),
            Some(&newly_active_destination),
        )
        .unwrap();

        assert_eq!(resolved.paths, drag.paths);
        assert_eq!(resolved.destination, newly_active_destination);
    }

    #[test]
    fn multi_selection_payload_and_all_changed_directories_are_preserved() {
        let paths = vec![
            PathBuf::from("/source-one/a.txt"),
            PathBuf::from("/source-one/b.txt"),
            PathBuf::from("/source-two/c.txt"),
        ];
        let drag = left_file_drag(paths.clone());
        let destination = PathBuf::from("/destination/exact-folder");
        let resolved = resolve_internal_file_drop(
            None,
            Some(&drag),
            Some(PaneSide::Right),
            &PathBuf::from("/left"),
            Some(&destination),
        )
        .unwrap();

        assert_eq!(resolved.paths, paths);
        assert_eq!(
            internal_drop_reload_dirs(&resolved.paths, &resolved.destination),
            vec![
                destination,
                PathBuf::from("/source-one"),
                PathBuf::from("/source-two"),
            ]
        );
    }

    #[test]
    fn drag_started_is_returned_without_being_attached_to_a_tab() {
        let paths = vec![
            PathBuf::from("/source/a.txt"),
            PathBuf::from("/source/b.txt"),
        ];
        let mut bookmarks = crate::core::bookmarks::Bookmarks::default();
        let mut toasts = crate::ui::toasts::Toasts::default();

        let outcome = super::App::handle_file_actions(
            vec![crate::ui::file_list::FileListAction::DragStarted(
                paths.clone(),
            )],
            &mut bookmarks,
            &mut toasts,
            crate::core::config::TerminalApp::Auto,
        );

        assert_eq!(outcome.drag_started, Some(paths));
        assert!(outcome.internal_drop.is_none());
    }

    #[test]
    fn one_action_batch_selects_only_one_explicit_drop() {
        let paths = vec![PathBuf::from("/source/item.txt")];
        let first_destination = PathBuf::from("/destination/folder-row");
        let mut bookmarks = crate::core::bookmarks::Bookmarks::default();
        let mut toasts = crate::ui::toasts::Toasts::default();

        let outcome = super::App::handle_file_actions(
            vec![
                crate::ui::file_list::FileListAction::MoveItems(
                    paths.clone(),
                    first_destination.clone(),
                ),
                crate::ui::file_list::FileListAction::MoveItems(
                    paths.clone(),
                    PathBuf::from("/destination/pane-root"),
                ),
            ],
            &mut bookmarks,
            &mut toasts,
            crate::core::config::TerminalApp::Auto,
        );

        assert_eq!(
            outcome.internal_drop,
            Some(InternalFileDrop {
                paths,
                destination: first_destination,
            })
        );
    }

    #[test]
    fn file_shortcuts_are_blocked_by_text_or_terminal_focus() {
        assert!(file_list_owns_keyboard_commands(false, false));
        assert!(!file_list_owns_keyboard_commands(true, false));
        assert!(!file_list_owns_keyboard_commands(false, true));
        assert!(!file_list_owns_keyboard_commands(true, true));
    }

    fn tab_cycle_event(
        key: egui::Key,
        physical_key: Option<egui::Key>,
        repeat: bool,
    ) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key,
            pressed: true,
            repeat,
            modifiers: egui::Modifiers {
                shift: true,
                mac_cmd: true,
                command: true,
                ..Default::default()
            },
        }
    }

    #[test]
    fn tab_cycle_wraps_in_both_directions() {
        assert_eq!(cycle_tab_index(0, 4, TabCycleDirection::Previous), 3);
        assert_eq!(cycle_tab_index(3, 4, TabCycleDirection::Next), 0);
        assert_eq!(cycle_tab_index(2, 4, TabCycleDirection::Previous), 1);
        assert_eq!(cycle_tab_index(1, 4, TabCycleDirection::Next), 2);
    }

    #[test]
    fn tab_cycle_is_stable_with_zero_or_one_tab() {
        assert_eq!(cycle_tab_index(0, 0, TabCycleDirection::Previous), 0);
        assert_eq!(cycle_tab_index(0, 1, TabCycleDirection::Next), 0);
    }

    #[test]
    fn tab_cycle_accepts_logical_curly_and_physical_bracket_keys() {
        assert_eq!(
            tab_cycle_key_event(&tab_cycle_event(
                egui::Key::OpenCurlyBracket,
                None,
                false,
            )),
            Some((TabCycleDirection::Previous, false))
        );
        assert_eq!(
            tab_cycle_key_event(&tab_cycle_event(
                egui::Key::A,
                Some(egui::Key::OpenBracket),
                false,
            )),
            Some((TabCycleDirection::Previous, false))
        );
        assert_eq!(
            tab_cycle_key_event(&tab_cycle_event(
                egui::Key::CloseCurlyBracket,
                None,
                false,
            )),
            Some((TabCycleDirection::Next, false))
        );
        assert_eq!(
            tab_cycle_key_event(&tab_cycle_event(
                egui::Key::A,
                Some(egui::Key::CloseBracket),
                false,
            )),
            Some((TabCycleDirection::Next, false))
        );
    }

    #[test]
    fn tab_cycle_requires_command_and_shift() {
        let mut event = tab_cycle_event(egui::Key::OpenCurlyBracket, None, false);
        let egui::Event::Key { modifiers, .. } = &mut event else {
            unreachable!();
        };
        modifiers.shift = false;

        assert_eq!(tab_cycle_key_event(&event), None);
    }

    #[test]
    fn tab_cycle_shortcut_is_consumed_before_terminal_input() {
        let mut events = vec![
            egui::Event::Text("unrelated".to_string()),
            tab_cycle_event(egui::Key::CloseCurlyBracket, None, false),
        ];

        assert_eq!(
            take_tab_cycle_shortcut(&mut events),
            Some(TabCycleDirection::Next)
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], egui::Event::Text(text) if text == "unrelated"));
    }

    #[test]
    fn repeated_tab_cycle_key_is_consumed_without_cycling() {
        let mut events = vec![tab_cycle_event(
            egui::Key::OpenCurlyBracket,
            None,
            true,
        )];

        assert_eq!(take_tab_cycle_shortcut(&mut events), None);
        assert!(events.is_empty());
    }

    #[test]
    fn affected_directories_are_deduplicated() {
        let mut dirs = vec![PathBuf::from("/one")];
        push_unique_path(&mut dirs, PathBuf::from("/one"));
        push_unique_path(&mut dirs, PathBuf::from("/two"));
        assert_eq!(dirs, vec![PathBuf::from("/one"), PathBuf::from("/two")]);
    }

    #[test]
    fn changing_tag_view_clears_stale_selection() {
        let root = TestDir::new("tag-view-selection");
        let selected = root.0.join("selected.txt");
        std::fs::write(&selected, "selected").unwrap();
        let mut tab = TabState::new(root.0.clone(), false);
        tab.list_state.select_only(selected);

        let result = root.0.join("tagged.txt");
        tab.set_tag_view(Some("Red".to_string()), Some(vec![result.clone()]));

        assert!(tab.list_state.selected.is_empty());
        assert!(tab.list_state.selection_anchor.is_none());
        assert_eq!(tab.tag_filter.as_deref(), Some("Red"));
        assert_eq!(tab.visible_entry_paths(), vec![result]);
    }

    #[test]
    fn external_directory_rename_follows_the_folder_and_selection() {
        let root = TestDir::new("watched-directory-rename");
        let old_directory = root.0.join("old");
        let new_directory = root.0.join("new");
        std::fs::create_dir(&old_directory).unwrap();
        let old_file = old_directory.join("selected.txt");
        std::fs::write(&old_file, "selected").unwrap();
        let mut tab = TabState::new(old_directory.clone(), false);
        tab.list_state.select_only(old_file);
        std::fs::rename(&old_directory, &new_directory).unwrap();

        assert!(tab.follow_external_directory_rename(
            &old_directory,
            &new_directory,
            false,
        ));
        assert_eq!(tab.current_path, new_directory);
        let expected_selection = tab.current_path.join("selected.txt");
        assert_eq!(
            tab.list_state.primary_selection(),
            Some(&expected_selection),
        );
        assert_eq!(tab.entries.len(), 1);
    }

    #[test]
    fn removed_current_directory_falls_back_to_existing_parent() {
        let root = TestDir::new("watched-directory-remove");
        let removed = root.0.join("removed");
        std::fs::create_dir(&removed).unwrap();
        let mut tab = TabState::new(removed.clone(), false);
        std::fs::remove_dir(&removed).unwrap();

        assert!(matches!(
            tab.reload(false),
            super::ReloadOutcome::CurrentDirectoryMissing,
        ));
        assert_eq!(
            tab.recover_missing_directory(false),
            Some((removed, root.0.clone())),
        );
        assert_eq!(tab.current_path, root.0);
        assert!(tab.list_state.selected.is_empty());
    }

    #[test]
    fn browser_session_restores_tab_order_active_tabs_and_layout() {
        let root = TestDir::new("session-layout");
        let first = root.0.join("first");
        let second = root.0.join("한글 folder");
        let right = root.0.join("right");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::create_dir(&right).unwrap();
        let saved = SessionState {
            version: SESSION_VERSION,
            left: PaneSession {
                tabs: vec![
                    TabSession {
                        path: first.clone(),
                    },
                    TabSession {
                        path: second.clone(),
                    },
                ],
                active_tab: 1,
            },
            right: Some(PaneSession {
                tabs: vec![TabSession {
                    path: right.clone(),
                }],
                active_tab: 0,
            }),
            focus: SavedPaneSide::Right,
            split_ratio: 0.37,
        };

        let restored = restore_browser_session(Some(&saved), &root.0, false);

        assert_eq!(
            restored
                .left
                .tabs
                .iter()
                .map(|tab| tab.current_path.clone())
                .collect::<Vec<_>>(),
            vec![first, second],
        );
        assert_eq!(restored.left.active_tab, 1);
        assert_eq!(restored.right.as_ref().unwrap().active_tab, 0);
        assert_eq!(restored.right.as_ref().unwrap().tabs[0].current_path, right);
        assert!(restored.focus == PaneSide::Right);
        assert_eq!(restored.split_ratio, 0.37);
        assert_eq!(
            browser_session_snapshot(
                &restored.left,
                restored.right.as_ref(),
                restored.focus,
                restored.split_ratio,
            ),
            saved,
        );
    }

    #[test]
    fn browser_session_repairs_missing_paths_and_invalid_layout() {
        let root = TestDir::new("session-repair");
        let missing = root.0.join("removed").join("nested");
        let saved = SessionState {
            version: SESSION_VERSION,
            left: PaneSession {
                tabs: vec![TabSession { path: missing }],
                active_tab: 99,
            },
            right: Some(PaneSession {
                tabs: Vec::new(),
                active_tab: 22,
            }),
            focus: SavedPaneSide::Right,
            split_ratio: f32::INFINITY,
        };

        let restored = restore_browser_session(Some(&saved), &root.0, false);

        assert_eq!(restored.left.tabs.len(), 1);
        assert_eq!(restored.left.tabs[0].current_path, root.0);
        assert_eq!(restored.left.active_tab, 0);
        assert!(restored.right.is_none());
        assert!(restored.focus == PaneSide::Left);
        assert_eq!(restored.split_ratio, 0.5);
    }

    #[test]
    fn session_snapshot_excludes_transient_selection_and_tag_state() {
        let root = TestDir::new("session-transient");
        let selected = root.0.join("selected.txt");
        std::fs::write(&selected, "selected").unwrap();
        let mut pane = PaneState::new(root.0.clone(), false);
        let before = browser_session_snapshot(&pane, None, PaneSide::Left, 0.5);

        pane.active_mut().list_state.select_only(selected);
        pane.active_mut().tag_filter = Some("Red".to_string());
        let after = browser_session_snapshot(&pane, None, PaneSide::Left, 0.5);

        assert_eq!(before, after);
    }

    #[test]
    fn session_save_debounce_tracks_the_newest_layout_and_cancels_reverted_changes() {
        let root = TestDir::new("session-debounce");
        let pane = PaneState::new(root.0.clone(), false);
        let saved = browser_session_snapshot(&pane, None, PaneSide::Left, 0.5);
        let mut changed = saved.clone();
        changed.split_ratio = 0.6;
        let start = std::time::Instant::now();
        let mut pending = None;

        update_pending_session(Some(&saved), &mut pending, changed.clone(), start);
        assert_eq!(
            pending.as_ref().unwrap().deadline,
            start + SESSION_SAVE_DEBOUNCE,
        );

        let mut newest = changed;
        newest.split_ratio = 0.7;
        let later = start + std::time::Duration::from_millis(100);
        update_pending_session(Some(&saved), &mut pending, newest.clone(), later);
        assert_eq!(pending.as_ref().unwrap().snapshot, newest);
        assert_eq!(
            pending.as_ref().unwrap().deadline,
            later + SESSION_SAVE_DEBOUNCE,
        );

        update_pending_session(Some(&saved), &mut pending, saved.clone(), later);
        assert!(pending.is_none());
    }
}

use std::path::PathBuf;
use std::sync::Arc;
use eframe::egui;

use crate::core::{
    bookmarks::Bookmarks,
    config::AppConfig,
    fs::{read_dir, sort_entries, FileEntry},
    global_tags::GlobalTags,
    search::SearchEngine,
};
use crate::git::{repo as git_repo, graph as git_graph, diff as git_diff, operations as git_ops};
use crate::platform::{clipboard, opener, quicklook, share};
use crate::core::terminal::TerminalState;
use crate::ui::{
    file_list::{self, FileListAction, FileListState},
    git_panel::{self, GitPanelAction, GitPanelState},
    sidebar::{self, SidebarAction},
    search_overlay,
    tab_bar,
    terminal_panel::{self, TerminalPanelEvent},
    toasts::Toasts,
    prefs,
};

// ── Per-tab state ─────────────────────────────────────────────────────────────

pub struct TabState {
    pub current_path: PathBuf,
    pub entries: Vec<FileEntry>,
    pub list_state: FileListState,
    pub dragging_path: Option<PathBuf>,
    pub tag_filter: Option<String>,
    pub tag_search_results: Option<Vec<PathBuf>>,
    history: Vec<PathBuf>,
    history_pos: usize,
}

impl TabState {
    pub fn new(path: PathBuf, show_hidden: bool) -> Self {
        let mut tab = Self {
            current_path: path.clone(),
            entries: Vec::new(),
            list_state: FileListState::default(),
            dragging_path: None,
            tag_filter: None,
            tag_search_results: None,
            history: vec![path],
            history_pos: 0,
        };
        tab.reload(show_hidden);
        tab
    }

    pub fn reload(&mut self, show_hidden: bool) {
        let sel = self.list_state.selected.clone();
        self.entries = read_dir(&self.current_path, show_hidden);
        sort_entries(&mut self.entries, self.list_state.sort_col, self.list_state.sort_order);
        // Restore selection if the path still exists; clear otherwise.
        self.list_state.selected = sel.filter(|p| self.entries.iter().any(|e| &e.path == p));
    }

    pub fn navigate(&mut self, path: PathBuf, show_hidden: bool) -> bool {
        if path.is_dir() {
            // Discard any forward history when navigating to a new location.
            self.history.truncate(self.history_pos + 1);
            self.history.push(path.clone());
            self.history_pos = self.history.len() - 1;
            self.current_path = path;
            // Tag filter and in-progress rename/create are scoped to a directory.
            self.tag_filter = None;
            self.tag_search_results = None;
            self.list_state.renaming = None;
            self.list_state.creating = None;
            self.reload(show_hidden);
            true
        } else {
            false
        }
    }

    pub fn can_go_back(&self) -> bool {
        self.history_pos > 0
    }

    pub fn go_back(&mut self, show_hidden: bool) {
        if self.history_pos > 0 {
            self.history_pos -= 1;
            self.current_path = self.history[self.history_pos].clone();
            self.reload(show_hidden);
        }
    }

    pub fn name(&self) -> String {
        self.current_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".to_string())
    }
}

// ── Pane (one tab group) ──────────────────────────────────────────────────────

pub struct PaneState {
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
}

impl PaneState {
    pub fn new(path: PathBuf, show_hidden: bool) -> Self {
        Self { tabs: vec![TabState::new(path, show_hidden)], active_tab: 0 }
    }

    pub fn active(&self) -> &TabState { &self.tabs[self.active_tab] }
    pub fn active_mut(&mut self) -> &mut TabState { &mut self.tabs[self.active_tab] }

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
        if self.tabs.len() == 1 { return true; }
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() { self.active_tab = self.tabs.len() - 1; }
        false
    }

    /// Remove and return tab at `idx`. Returns None if it's the last tab.
    pub fn take_tab(&mut self, idx: usize) -> Option<TabState> {
        if self.tabs.len() == 1 { return None; }
        let tab = self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() { self.active_tab = self.tabs.len() - 1; }
        Some(tab)
    }

    pub fn add_tab(&mut self, tab: TabState) {
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    pub fn reload_all(&mut self, show_hidden: bool) {
        for t in &mut self.tabs { t.reload(show_hidden); }
    }
}

// ── File clipboard ────────────────────────────────────────────────────────────

#[derive(Clone)]
enum ClipboardOp {
    Copy(PathBuf),
    Cut(PathBuf),
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
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name")
    })?;
    let dest = to_dir.join(file_name);
    std::fs::rename(from, &dest).or_else(|_| -> std::io::Result<()> {
        copy_path_recursive(from, &dest)?;
        if from.is_dir() { std::fs::remove_dir_all(from) } else { std::fs::remove_file(from) }
    })?;
    Ok(dest)
}

// ── Focus / drag ──────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
pub enum PaneSide { Left, Right }

struct TabDrag { from: PaneSide, tab_idx: usize }

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
    // ── Terminal panel ────────────────────────────────────────────────────────
    terminal_open: bool,
    terminals: Vec<TerminalState>,
    terminal_active: usize,
    terminal_last_sync_path: Option<PathBuf>,
    // ── External drag ─────────────────────────────────────────────────────────
    #[cfg(target_os = "macos")]
    external_drag_active: bool,
    #[cfg(target_os = "macos")]
    pending_reload: Option<(std::time::Instant, Vec<std::path::PathBuf>)>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let config = AppConfig::load();
        apply_theme(&cc.egui_ctx, config.theme);
        crate::platform::fonts::setup_fonts(&cc.egui_ctx);
        let bookmarks = Bookmarks::load();
        let start_path = config
            .last_path
            .clone()
            .filter(|p| p.exists())
            .unwrap_or_else(|| {
                PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()))
            });

        Self {
            left: PaneState::new(start_path, config.show_hidden),
            right: None,
            focus: PaneSide::Left,
            tab_drag: None,
            split_ratio: 0.5,
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
            terminal_open: false,
            terminals: Vec::new(),
            terminal_active: 0,
            terminal_last_sync_path: None,
            #[cfg(target_os = "macos")]
            external_drag_active: false,
            #[cfg(target_os = "macos")]
            pending_reload: None,
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
                    Ok(t) => { self.terminals.push(t); self.terminal_active = 0; }
                    Err(_) => { return; }
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
                self.right = Some(PaneState { tabs: vec![tab], active_tab: 0 });
            }
        } else {
            // Last tab on left — clone the path to right (left must keep ≥1 tab)
            let path = self.left.tabs[0].current_path.clone();
            let hidden = self.config.show_hidden;
            let tab = TabState::new(path, hidden);
            if let Some(r) = &mut self.right {
                r.add_tab(tab);
            } else {
                self.right = Some(PaneState { tabs: vec![tab], active_tab: 0 });
            }
        }
        self.focus = PaneSide::Right;
    }

    fn move_tab_to_left(&mut self, tab_idx: usize) {
        if let Some(r) = &mut self.right {
            if let Some(tab) = r.take_tab(tab_idx) {
                self.left.add_tab(tab);
                if r.tabs.is_empty() { self.right = None; }
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
        dragging_path: &mut Option<PathBuf>,
    ) -> (Option<PathBuf>, bool, Option<PathBuf>, Option<PathBuf>, bool) {
        let mut navigate_to: Option<PathBuf> = None;
        let mut reload_needed = false;
        let mut quicklook_path: Option<PathBuf> = None;
        let mut select_after_nav: Option<PathBuf> = None;
        let mut clear_tag_filter = false;
        for action in actions {
            match action {
                FileListAction::Navigate(path) => navigate_to = Some(path),
                FileListAction::NavigateAndSelect(dir, file) => {
                    navigate_to = Some(dir);
                    select_after_nav = Some(file);
                }
                FileListAction::ClearTagFilter => { clear_tag_filter = true; }
                FileListAction::OpenFile(path) => opener::open_file(&path),
                FileListAction::CopyPath(path) => {
                    clipboard::copy_path(&path);
                    toasts.push(format!("Copied: {}", path.to_string_lossy()));
                }
                FileListAction::AddBookmark(path) => { bookmarks.add(path); }
                FileListAction::RevealInFinder(path) => opener::reveal_in_finder(&path),
                FileListAction::OpenInTerminal(path) => opener::open_in_terminal(&path, terminal),
                FileListAction::DragStarted(path) => { *dragging_path = Some(path); }
                FileListAction::QuickLook(path) => quicklook_path = Some(path),
                FileListAction::Share(path) => share::show_share_sheet(&path),
                FileListAction::GetInfo(path) => opener::get_info(&path),
                FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) => {}
                FileListAction::StartRename(_) | FileListAction::RenameItem(_, _) => {}
                FileListAction::CopyFile(_) | FileListAction::CutFile(_) | FileListAction::PasteHere => {}
                FileListAction::DeleteFile(path) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    match opener::trash_path(&path) {
                        Ok(_) => {
                            toasts.push(format!("Moved to Trash: {}", name));
                            reload_needed = true;
                        }
                        Err(e) => { toasts.push(format!("Trash failed: {}", e)); }
                    }
                }
                FileListAction::MoveItem(from, to_dir) => {
                    match move_path(&from, &to_dir) {
                        Ok(_) => { reload_needed = true; }
                        Err(e) => { toasts.push(format!("Move failed: {}", e)); reload_needed = true; }
                    }
                }
                FileListAction::SetTags(path, new_tags) => {
                    crate::core::tags::write_tags(&path, &new_tags);
                    reload_needed = true;
                    let file_name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    if new_tags.is_empty() {
                        toasts.push(format!("Removed all tags from {}", file_name));
                    } else {
                        let names: Vec<&str> = new_tags.iter().map(|t| t.name.as_str()).collect();
                        toasts.push(format!("{}: {}", file_name, names.join(", ")));
                    }
                }
            }
        }
        (navigate_to, reload_needed, quicklook_path, select_after_nav, clear_tag_filter)
    }

    fn handle_creating_actions(
        actions: Vec<FileListAction>,
        tab: &mut TabState,
        toasts: &mut Toasts,
    ) -> (bool, Option<PathBuf>) {
        let mut reload_needed = false;
        let mut select_after: Option<PathBuf> = None;
        for action in actions {
            match action {
                FileListAction::StartCreating(kind) => {
                    let name = match kind {
                        file_list::CreateKind::File => "untitled".to_string(),
                        file_list::CreateKind::Directory => "untitled folder".to_string(),
                    };
                    tab.list_state.creating = Some(file_list::CreatingItem { kind, name, needs_focus: true });
                }
                FileListAction::CreateItem(kind, name) => {
                    let target = tab.current_path.join(&name);
                    match kind {
                        file_list::CreateKind::File => { let _ = std::fs::File::create(&target); }
                        file_list::CreateKind::Directory => { let _ = std::fs::create_dir(&target); }
                    }
                    reload_needed = true;
                    select_after = Some(target);
                }
                FileListAction::StartRename(path) => {
                    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                    tab.list_state.selected = Some(path.clone());
                    tab.list_state.renaming = Some(file_list::RenamingItem {
                        path,
                        name,
                        needs_focus: true,
                    });
                }
                FileListAction::RenameItem(old_path, new_name) => {
                    let new_path = old_path.parent().unwrap_or(&old_path).join(&new_name);
                    match std::fs::rename(&old_path, &new_path) {
                        Ok(_) => {
                            reload_needed = true;
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
        (reload_needed, select_after)
    }

    fn do_paste_op(
        op: ClipboardOp,
        dest_dir: &PathBuf,
        clipboard_op: &mut Option<ClipboardOp>,
        toasts: &mut Toasts,
    ) -> (bool, Option<PathBuf>) {
        let (src, is_move) = match &op {
            ClipboardOp::Copy(p) => (p.clone(), false),
            ClipboardOp::Cut(p) => (p.clone(), true),
        };
        let file_name = match src.file_name() {
            Some(n) => n.to_os_string(),
            None => { toasts.push("Cannot paste: no file name".to_string()); return (false, None); }
        };
        let dest = dest_dir.join(&file_name);
        if dest == src {
            toasts.push("Source and destination are the same".to_string());
            return (false, None);
        }
        let result: std::io::Result<()> = if is_move {
            std::fs::rename(&src, &dest).or_else(|_| -> std::io::Result<()> {
                copy_path_recursive(&src, &dest)?;
                if src.is_dir() { std::fs::remove_dir_all(&src) } else { std::fs::remove_file(&src) }
            })
        } else {
            copy_path_recursive(&src, &dest)
        };
        let name = file_name.to_string_lossy().to_string();
        match result {
            Ok(_) => {
                if is_move { *clipboard_op = None; }
                toasts.push(if is_move { format!("Moved: {}", name) } else { format!("Pasted: {}", name) });
                (true, Some(dest))
            }
            Err(e) => {
                toasts.push(format!("Paste failed: {}", e));
                (false, None)
            }
        }
    }

    fn handle_clipboard_actions(
        actions: Vec<FileListAction>,
        clipboard_op: &mut Option<ClipboardOp>,
        paste_dir: &PathBuf,
        toasts: &mut Toasts,
    ) -> (bool, Option<PathBuf>) {
        let mut reload_needed = false;
        let mut select_after: Option<PathBuf> = None;
        for action in actions {
            match action {
                FileListAction::CopyFile(p) => {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                    *clipboard_op = Some(ClipboardOp::Copy(p));
                    toasts.push(format!("Copied: {}", name));
                }
                FileListAction::CutFile(p) => {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
                    *clipboard_op = Some(ClipboardOp::Cut(p));
                    toasts.push(format!("Cut: {}", name));
                }
                FileListAction::PasteHere => {
                    if let Some(op) = clipboard_op.clone() {
                        let (r, s) = Self::do_paste_op(op, paste_dir, clipboard_op, toasts);
                        if r { reload_needed = true; }
                        if let Some(p) = s { select_after = Some(p); }
                    }
                }
                _ => {}
            }
        }
        (reload_needed, select_after)
    }

    fn do_quicklook(&mut self, path: PathBuf) {
        quicklook::open_quicklook(&path);
    }

    fn handle_git_actions(&mut self, actions: Vec<GitPanelAction>, wd: &PathBuf) {
        let mut needs_refresh = false;
        let mut needs_dir_reload = false;

        for action in actions {
            match action {
                GitPanelAction::Refresh => { needs_refresh = true; }
                GitPanelAction::SwitchTab(_) => {}

                GitPanelAction::StageFile(path) => {
                    match git_ops::stage_file(wd, &path) {
                        Ok(_) => { needs_refresh = true; }
                        Err(e) => { self.toasts.push(format!("Stage failed: {}", e)); }
                    }
                }
                GitPanelAction::UnstageFile(path) => {
                    match git_ops::unstage_file(wd, &path) {
                        Ok(_) => { needs_refresh = true; }
                        Err(e) => { self.toasts.push(format!("Unstage failed: {}", e)); }
                    }
                }
                GitPanelAction::StageAll => {
                    match git_ops::stage_all(wd) {
                        Ok(_) => { needs_refresh = true; }
                        Err(e) => { self.toasts.push(format!("Stage all failed: {}", e)); }
                    }
                }
                GitPanelAction::UnstageAll => {
                    match git_ops::unstage_all(wd) {
                        Ok(_) => { needs_refresh = true; }
                        Err(e) => { self.toasts.push(format!("Unstage all failed: {}", e)); }
                    }
                }
                GitPanelAction::SelectDiff(path, staged) => {
                    self.git_panel.diff = git_diff::get_file_diff(wd, &path, staged);
                    self.git_panel.diff_file = Some((path, staged));
                }
                GitPanelAction::Commit(msg) => {
                    match git_ops::commit(wd, &msg) {
                        Ok(_) => {
                            self.git_panel.commit_msg.clear();
                            self.toasts.push("Committed.");
                            needs_refresh = true;
                        }
                        Err(e) => { self.toasts.push(format!("Commit failed: {}", e)); }
                    }
                }
                GitPanelAction::CheckoutBranch(name) => {
                    match git_ops::checkout_branch(wd, &name) {
                        Ok(_) => {
                            needs_refresh = true;
                            needs_dir_reload = true;
                            self.toasts.push(format!("Checked out '{}'", name));
                        }
                        Err(e) => { self.toasts.push(format!("Checkout failed: {}", e)); }
                    }
                }
                GitPanelAction::CreateBranch(name) => {
                    match git_ops::create_branch(wd, &name) {
                        Ok(_) => {
                            self.git_panel.new_branch_name.clear();
                            needs_refresh = true;
                            self.toasts.push(format!("Created branch '{}'", name));
                        }
                        Err(e) => { self.toasts.push(format!("Create branch failed: {}", e)); }
                    }
                }
                GitPanelAction::DeleteBranch(name) => {
                    match git_ops::delete_branch(wd, &name) {
                        Ok(_) => {
                            needs_refresh = true;
                            self.toasts.push(format!("Deleted branch '{}'", name));
                        }
                        Err(e) => { self.toasts.push(format!("Delete branch failed: {}", e)); }
                    }
                }
                GitPanelAction::StashSave => {
                    match git_ops::stash_save(wd) {
                        Ok(_) => {
                            needs_refresh = true;
                            needs_dir_reload = true;
                            self.toasts.push("Stashed working changes.");
                        }
                        Err(e) => { self.toasts.push(format!("Stash failed: {}", e)); }
                    }
                }
                GitPanelAction::StashApply(idx) => {
                    match git_ops::stash_apply(wd, idx) {
                        Ok(_) => {
                            needs_refresh = true;
                            needs_dir_reload = true;
                            self.toasts.push(format!("Applied stash [{}]", idx));
                        }
                        Err(e) => { self.toasts.push(format!("Stash apply failed: {}", e)); }
                    }
                }
                GitPanelAction::StashDrop(idx) => {
                    match git_ops::stash_drop(wd, idx) {
                        Ok(_) => {
                            needs_refresh = true;
                            self.toasts.push(format!("Dropped stash [{}]", idx));
                        }
                        Err(e) => { self.toasts.push(format!("Stash drop failed: {}", e)); }
                    }
                }
                GitPanelAction::Fetch => {
                    let remote = self.git_panel.remote_name.clone();
                    match git_ops::fetch(wd, &remote) {
                        Ok(msg) => {
                            self.git_panel.op_log.push(format!("fetch {}: {}", remote, msg));
                            needs_refresh = true;
                        }
                        Err(e) => { self.git_panel.op_log.push(format!("fetch error: {}", e)); }
                    }
                }
                GitPanelAction::Pull => {
                    let remote = self.git_panel.remote_name.clone();
                    match git_ops::pull(wd, &remote) {
                        Ok(msg) => {
                            self.git_panel.op_log.push(format!("pull {}: {}", remote, msg));
                            needs_refresh = true;
                            needs_dir_reload = true;
                        }
                        Err(e) => { self.git_panel.op_log.push(format!("pull error: {}", e)); }
                    }
                }
                GitPanelAction::Push => {
                    let remote = self.git_panel.remote_name.clone();
                    // Get current branch name from status
                    let branch = self.git_panel.status.as_ref()
                        .and_then(|s| s.head_branch.clone())
                        .unwrap_or_else(|| "main".to_string());
                    match git_ops::push_branch(wd, &remote, &branch) {
                        Ok(msg) => {
                            self.git_panel.op_log.push(format!("push {}/{}: {}", remote, branch, msg));
                        }
                        Err(e) => { self.git_panel.op_log.push(format!("push error: {}", e)); }
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

        if needs_refresh { self.refresh_git(); }
        if needs_dir_reload {
            let h = self.config.show_hidden;
            self.focused_pane_mut().active_mut().reload(h);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let pointer_released = ctx.input(|i| i.pointer.any_released());

        // ── External drag-end reload ──────────────────────────────────────────
        #[cfg(target_os = "macos")]
        {
            let now_active = crate::platform::drag::is_drag_active();
            let just_ended = self.external_drag_active && !now_active;
            self.external_drag_active = now_active;

            if just_ended {
                ctx.request_repaint();
            }

            if let Some((op, _paths)) = crate::platform::drag::take_drag_ended_op() {
                eprintln!("[app] external drag ended op={op}");
                if op != 0 {
                    // op=0 → Stop → skip reload, file stays. op=16 → Finder did rename → source gone.
                    self.pending_reload = Some((
                        std::time::Instant::now() + std::time::Duration::from_millis(300),
                        _paths,
                    ));
                    ctx.request_repaint_after(std::time::Duration::from_millis(300));
                }
            }

            let reload_due = self.pending_reload.as_ref()
                .map_or(false, |(d, _)| std::time::Instant::now() >= *d);
            if reload_due {
                if let Some((_, paths)) = self.pending_reload.take() {
                    let h = self.config.show_hidden;
                    // Reload every tab (in both panes) that is showing the directory
                    // from which files were dragged — not just whichever tab is active now.
                    let source_dirs: Vec<std::path::PathBuf> = paths.iter()
                        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
                        .collect();
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
        let file_dragging_name = self.left.active().dragging_path.as_ref()
            .or_else(|| self.right.as_ref().and_then(|r| r.active().dragging_path.as_ref()))
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string());

        if let Some(name) = &file_dragging_name {
            if let Some(pos) = ctx.pointer_hover_pos() {
                egui::show_tooltip_at(
                    ctx,
                    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("drag_ghost")),
                    egui::Id::new("drag_ghost"),
                    pos + egui::vec2(12.0, 4.0),
                    |ui| { ui.label(format!("📁 {}", name)); },
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
            let name = pane.tabs.get(drag.tab_idx).map(|t| t.name()).unwrap_or_default();
            if let Some(pos) = ctx.pointer_hover_pos() {
                egui::show_tooltip_at(
                    ctx,
                    egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("tab_drag_ghost")),
                    egui::Id::new("tab_drag_ghost"),
                    pos + egui::vec2(12.0, 4.0),
                    |ui| { ui.label(format!("⬜ {}", name)); },
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
        let mut kb_open_search = false;
        let mut kb_toggle_git = false;
        let mut kb_toggle_terminal = false;
        let mut kb_open_prefs = false;
        let mut kb_quicklook = false;
        let mut kb_copy_file = false;
        let mut kb_cut_file = false;
        let mut kb_paste_file = false;
        let mut kb_delete_file = false;

        // Bare-key shortcuts (no modifier) must not fire while a text field has focus,
        // otherwise Backspace in a TextEdit would also navigate up a directory.
        // wants_keyboard_input() is true only when a TextEdit is actively receiving text,
        // NOT just because an interactive widget (like a file row) has focus.
        let no_text_focus = !ctx.wants_keyboard_input();

        ctx.input(|i| {
            if i.modifiers.command && i.key_pressed(egui::Key::T) { kb_new_tab = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::W) { kb_close_tab = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::Backslash) { kb_close_right = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::F) { kb_open_search = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::G) { kb_toggle_git = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::J) { kb_toggle_terminal = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::N) { kb_new_window = true; }
            if i.modifiers.command && i.key_pressed(egui::Key::Comma) { kb_open_prefs = true; }
            // Backspace = go up; Cmd+Backspace = move to Trash
            if no_text_focus && i.key_pressed(egui::Key::Backspace) {
                if i.modifiers.command { kb_delete_file = true; } else { kb_go_up = true; }
            }
            // egui-winit converts Cmd+C/X/V on macOS to Event::Copy/Cut/Paste,
            // so key_pressed(Key::C) never fires. Check the semantic events instead.
            for event in &i.events {
                match event {
                    egui::Event::Copy if no_text_focus => kb_copy_file = true,
                    egui::Event::Cut if no_text_focus => kb_cut_file = true,
                    egui::Event::Paste(_) if no_text_focus => kb_paste_file = true,
                    _ => {}
                }
            }
            if i.modifiers.command && i.key_pressed(egui::Key::R) { kb_reload = true; }
            if i.modifiers.command && i.modifiers.shift && i.key_pressed(egui::Key::Period) {
                kb_toggle_hidden = true;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::Backtick) { kb_switch_focus = true; }
            for n in 1..=9usize {
                let key = match n {
                    1 => egui::Key::Num1, 2 => egui::Key::Num2, 3 => egui::Key::Num3,
                    4 => egui::Key::Num4, 5 => egui::Key::Num5, 6 => egui::Key::Num6,
                    7 => egui::Key::Num7, 8 => egui::Key::Num8, 9 => egui::Key::Num9,
                    _ => unreachable!(),
                };
                if i.modifiers.command && i.key_pressed(key) { kb_switch_tab = Some(n - 1); }
            }
        });

        // Space must be consumed via input_mut so the ScrollArea never sees it.
        ctx.input_mut(|i| {
            if no_text_focus && i.consume_key(egui::Modifiers::NONE, egui::Key::Space) {
                kb_quicklook = true;
            }
        });

        if kb_quicklook {
            if let Some(path) = self.focused_pane().active().list_state.selected.clone() {
                self.do_quicklook(path);
            }
        }
        if kb_copy_file {
            if let Some(path) = self.focused_pane().active().list_state.selected.clone() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.clipboard_op = Some(ClipboardOp::Copy(path));
                self.toasts.push(format!("Copied: {}", name));
            }
        }
        if kb_cut_file {
            if let Some(path) = self.focused_pane().active().list_state.selected.clone() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                self.clipboard_op = Some(ClipboardOp::Cut(path));
                self.toasts.push(format!("Cut: {}", name));
            }
        }
        if kb_paste_file {
            if let Some(op) = self.clipboard_op.clone() {
                let dest_dir = self.focused_pane().active().current_path.clone();
                let (reload, select) = Self::do_paste_op(op, &dest_dir, &mut self.clipboard_op, &mut self.toasts);
                if reload {
                    let h = self.config.show_hidden;
                    self.focused_pane_mut().active_mut().reload(h);
                    if let Some(p) = select { self.focused_pane_mut().active_mut().list_state.selected = Some(p); }
                }
            }
        }
        if kb_delete_file {
            if let Some(path) = self.focused_pane().active().list_state.selected.clone() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
                match opener::trash_path(&path) {
                    Ok(_) => {
                        self.toasts.push(format!("Moved to Trash: {}", name));
                        let h = self.config.show_hidden;
                        self.focused_pane_mut().active_mut().list_state.selected = None;
                        self.focused_pane_mut().active_mut().reload(h);
                    }
                    Err(e) => { self.toasts.push(format!("Trash failed: {}", e)); }
                }
            }
        }
        if kb_new_tab { let h = self.config.show_hidden; self.focused_pane_mut().new_tab(h); }
        if kb_close_tab {
            let focused = self.focus;
            let ai = self.focused_pane().active_tab;
            match focused {
                PaneSide::Left => { self.left.close_tab(ai); }
                PaneSide::Right => {
                    if let Some(r) = &mut self.right {
                        if r.close_tab(ai) { self.right = None; self.focus = PaneSide::Left; }
                    }
                }
            }
        }
        if kb_close_right { self.right = None; self.focus = PaneSide::Left; self.tab_drag = None; }
        if kb_new_window { Self::open_new_window(); }
        if kb_open_prefs { self.prefs_open = true; }
        if kb_toggle_hidden {
            self.config.show_hidden = !self.config.show_hidden;
            self.config.save();
            let h = self.config.show_hidden;
            self.left.reload_all(h);
            if let Some(r) = &mut self.right { r.reload_all(h); }
        }
        if kb_reload { let h = self.config.show_hidden; self.focused_pane_mut().active_mut().reload(h); }
        if kb_go_up {
            let h = self.config.show_hidden;
            self.focused_pane_mut().active_mut().go_back(h);
        }
        if let Some(idx) = kb_switch_tab {
            let n = self.focused_pane().tabs.len();
            if idx < n { self.focused_pane_mut().active_tab = idx; }
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
        let maybe_new_cwd: Option<PathBuf> = self.terminals.get(self.terminal_active)
            .and_then(|t| t.grid.lock().ok())
            .and_then(|mut g| g.take_cwd_update());
        if let Some(new_cwd) = maybe_new_cwd {
            if new_cwd.exists() {
                let h = self.config.show_hidden;
                self.focused_pane_mut().active_mut().navigate(new_cwd.clone(), h);
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

            let root = self.search_engine.as_ref().map(|e| e.root.clone())
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
                        path.parent().map(|p| p.to_path_buf()).unwrap_or(path.clone())
                    };
                    let h = self.config.show_hidden;
                    self.focused_pane_mut().active_mut().navigate(dir.clone(), h);
                    // Pre-select the found file in the list
                    self.focused_pane_mut().active_mut().list_state.selected = Some(path.clone());
                    self.config.last_path = Some(dir);
                    self.config.save();
                    self.toasts.push(format!(
                        "Found: {}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    ));
                    self.search_open = false;
                    self.search_engine = None;
                }
                None => {}
            }
        }

        // ── Preferences window ───────────────────────────────────────────────
        if self.prefs_open {
            let old_theme = self.config.theme;
            let old_hidden = self.config.show_hidden;
            let result = prefs::show(ctx, &mut self.prefs_open, &mut self.config);
            if result.config_changed {
                if self.config.theme != old_theme {
                    apply_theme(ctx, self.config.theme);
                }
                if self.config.show_hidden != old_hidden {
                    let h = self.config.show_hidden;
                    self.left.reload_all(h);
                    if let Some(r) = &mut self.right { r.reload_all(h); }
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
                        if self.config.show_hidden { "" } else { " (hidden files excluded)" }
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
                    if ui.button("◀").on_hover_text("Go back (Backspace)").clicked() {
                        let h = self.config.show_hidden;
                        self.focused_pane_mut().active_mut().go_back(h);
                    }
                });
                if ui.button("↺").on_hover_text("Reload (⌘R)").clicked() {
                    let h = self.config.show_hidden;
                    self.focused_pane_mut().active_mut().reload(h);
                }
                ui.separator();
                let hidden_label = if self.config.show_hidden { "Hide Hidden" } else { "Show Hidden" };
                if ui.button(hidden_label).on_hover_text("Toggle hidden files (⌘⇧.)").clicked() {
                    self.config.show_hidden = !self.config.show_hidden;
                    self.config.save();
                    let h = self.config.show_hidden;
                    self.left.reload_all(h);
                    if let Some(r) = &mut self.right { r.reload_all(h); }
                }
                ui.separator();
                if ui.button("⊞ New Window").on_hover_text("Open new window (⌘N)").clicked() {
                    Self::open_new_window();
                }
                if self.right.is_some() {
                    ui.separator();
                    if ui.button("✕ Close Split").on_hover_text("Close right pane (⌘\\)").clicked() {
                        self.right = None;
                        self.focus = PaneSide::Left;
                        self.tab_drag = None;
                    }
                }
                if let Some(_wd) = &self.git_workdir {
                    ui.separator();
                    let git_label = if self.git_panel_open { "Git ▾" } else { "Git ▸" };
                    if ui.button(git_label).on_hover_text("Toggle git panel (⌘G)").clicked() {
                        self.git_panel_open = !self.git_panel_open;
                        if self.git_panel_open { self.refresh_git(); }
                    }
                }
                ui.separator();
                let term_label = if self.terminal_open { "Terminal ▾" } else { "Terminal ▸" };
                if ui.button(term_label).on_hover_text("Toggle terminal panel (⌘J)").clicked() {
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
                            let cwd = self.terminals.get(active)
                                .and_then(|t| t.grid.lock().ok())
                                .and_then(|g| g.cwd.clone())
                                .unwrap_or_else(|| fallback_cwd.clone());
                            opener::open_in_terminal(&cwd, terminal_app_pref);
                        }
                        Some(TerminalPanelEvent::NewTab) => {
                            let cwd = self.focused_pane().active().current_path.clone();
                            let ctx2 = ctx.clone();
                            if let Ok(t) = TerminalState::spawn(80, 24, &cwd, Arc::new(move || ctx2.request_repaint())) {
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
        let file_dragging_path = self.left.active().dragging_path.clone()
            .or_else(|| self.right.as_ref().and_then(|r| r.active().dragging_path.clone()));
        let current_path_for_sidebar = self.focused_pane().active().current_path.clone();
        let active_tag = self.focused_pane().active().tag_filter.clone();
        let cut_path: Option<PathBuf> = match &self.clipboard_op {
            Some(ClipboardOp::Cut(p)) => Some(p.clone()),
            _ => None,
        };
        let has_clipboard = self.clipboard_op.is_some();

        let is_tab_dragging = self.tab_drag.is_some();
        let mut git_right_actions: Vec<GitPanelAction> = Vec::new();

        egui::CentralPanel::default().show(ctx, |ui| {
            let total_rect = ui.available_rect_before_wrap();

            // ── Sidebar ───────────────────────────────────────────────────────
            let sb_w = self.config.sidebar_width;
            let sb_div_x = total_rect.min.x + sb_w;
            let sidebar_rect = egui::Rect::from_min_max(
                total_rect.min,
                egui::pos2(sb_div_x, total_rect.max.y),
            );
            // Full pane area starts after the 1 px divider line.
            let git_right_w = if self.git_panel_open && self.config.git_panel_right {
                let avail = (total_rect.width() - sb_w - 1.0 - 200.0).max(200.0);
                self.config.git_panel_width.clamp(200.0, avail)
            } else { 0.0 };
            let full_rect = egui::Rect::from_min_max(
                egui::pos2(sb_div_x + 1.0, total_rect.min.y),
                egui::pos2(total_rect.max.x - git_right_w, total_rect.max.y),
            );
            self.content_rect = full_rect;

            // Paint sidebar background explicitly so no gaps appear on resize.
            ui.painter().rect_filled(sidebar_rect, egui::CornerRadius::ZERO, ui.visuals().panel_fill);

            // Sidebar content.
            let mut sidebar_nav: Option<PathBuf> = None;
            let mut sidebar_bookmark: Option<PathBuf> = None;
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect).id_salt("sidebar"), |ui| {
                ui.set_clip_rect(sidebar_rect);
                egui::ScrollArea::vertical()
                    .drag_to_scroll(false)
                    .show(ui, |ui| {
                        for action in sidebar::show(
                            ui,
                            &mut self.bookmarks,
                            &self.global_tags,
                            &current_path_for_sidebar,
                            &file_dragging_path,
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
                                SidebarAction::MoveFileTo(from, to_dir) => {
                                    match move_path(&from, &to_dir) {
                                        Ok(_) => {
                                            let name = from.file_name().unwrap_or_default().to_string_lossy().to_string();
                                            self.toasts.push(format!("Moved: {}", name));
                                            let h = self.config.show_hidden;
                                            self.focused_pane_mut().active_mut().reload(h);
                                        }
                                        Err(e) => { self.toasts.push(format!("Move failed: {}", e)); }
                                    }
                                }
                                SidebarAction::FilterTag(tag) => {
                                    let tab = self.focused_pane_mut().active_mut();
                                    if let Some(ref name) = tag {
                                        let mut results = crate::core::global_tags::search_by_tag(name);
                                        // Spotlight may not have indexed recently-applied tags yet.
                                        // Merge in any matches from the currently loaded directory.
                                        for entry in &tab.entries {
                                            if entry.tags.iter().any(|t| &t.name == name)
                                                && !results.contains(&entry.path)
                                            {
                                                results.push(entry.path.clone());
                                            }
                                        }
                                        results.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
                                        tab.tag_search_results = Some(results);
                                    } else {
                                        tab.tag_search_results = None;
                                    }
                                    tab.tag_filter = tag;
                                }
                                SidebarAction::CreateTag(name, color) => {
                                    if !self.global_tags.add(name.clone(), color) {
                                        self.toasts.push(format!("Tag '{}' already exists", name));
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
            });

            if let Some(p) = sidebar_nav {
                let h = self.config.show_hidden;
                self.focused_pane_mut().active_mut().navigate(p.clone(), h);
                self.config.last_path = Some(p);
                self.config.save();
            }
            if let Some(p) = sidebar_bookmark {
                let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();
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
                    self.config.sidebar_width = (self.config.sidebar_width + div_resp.drag_delta().x).clamp(120.0, 480.0);
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
            ui.painter().vline(sb_div_x, total_rect.y_range(), egui::Stroke::new(1.0, sb_div_color));

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
                    self.config.terminal_panel_height = (self.config.terminal_panel_height - dy).clamp(80.0, 600.0);
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
                    self.config.git_panel_height = (self.config.git_panel_height - dy).clamp(80.0, 600.0);
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

                let div_id = ui.id().with("split_divider");
                let div_resp = ui.interact(div_rect, div_id, egui::Sense::drag());
                if div_resp.dragged() {
                    let dx = div_resp.drag_delta().x;
                    self.split_ratio = (self.split_ratio + dx / full_rect.width())
                        .clamp(0.15, 0.85);
                }
                if div_resp.hovered() || div_resp.dragged() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
                let div_color = if div_resp.hovered() || div_resp.dragged() {
                    ui.visuals().selection.bg_fill
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                };
                ui.painter().rect_filled(div_rect, egui::CornerRadius::ZERO, div_color);

                // ── Left pane ─────────────────────────────────────────────────
                let left_focus = self.focus == PaneSide::Left;
                let left_drop_target = is_tab_dragging
                    && self.tab_drag.as_ref().map_or(false, |d| d.from == PaneSide::Right);

                let (left_tab_actions, left_file_actions, left_focus_clicked) =
                    render_pane(ui, left_rect, &mut self.left, left_focus, left_drop_target, false, "left", &self.global_tags, cut_path.as_ref(), has_clipboard);

                for action in left_tab_actions {
                    match action {
                        tab_bar::TabBarAction::Switch(i) => { self.left.active_tab = i; self.focus = PaneSide::Left; }
                        tab_bar::TabBarAction::Close(i) => { self.left.close_tab(i); }
                        tab_bar::TabBarAction::New => { let h = self.config.show_hidden; self.left.new_tab(h); self.focus = PaneSide::Left; }
                        tab_bar::TabBarAction::DragTab(i) => { self.tab_drag = Some(TabDrag { from: PaneSide::Left, tab_idx: i }); }
                    }
                }
                let left_ai = self.left.active_tab;
                let terminal = self.config.terminal;
                let (left_special, left_regular): (Vec<_>, Vec<_>) = left_file_actions.into_iter()
                    .partition(|a| matches!(a, FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) | FileListAction::StartRename(_) | FileListAction::RenameItem(_, _) | FileListAction::CopyFile(_) | FileListAction::CutFile(_) | FileListAction::PasteHere));
                let (left_creating, left_clipboard): (Vec<_>, Vec<_>) = left_special.into_iter()
                    .partition(|a| matches!(a, FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) | FileListAction::StartRename(_) | FileListAction::RenameItem(_, _)));
                let (left_nav, left_reload, left_ql, left_sel_nav, left_clear_tag) = Self::handle_file_actions(
                    left_regular,
                    &mut self.bookmarks,
                    &mut self.toasts,
                    terminal,
                    &mut self.left.tabs[self.left.active_tab].dragging_path,
                );
                let (left_create_reload, left_create_sel) = Self::handle_creating_actions(left_creating, &mut self.left.tabs[left_ai], &mut self.toasts);
                let left_paste_dir = self.left.tabs[left_ai].current_path.clone();
                let (left_clip_reload, left_clip_sel) = Self::handle_clipboard_actions(left_clipboard, &mut self.clipboard_op, &left_paste_dir, &mut self.toasts);
                let left_select = left_create_sel.or(left_clip_sel);
                if left_reload || left_create_reload || left_clip_reload {
                    let h = self.config.show_hidden;
                    self.left.tabs[left_ai].reload(h);
                    if let Some(p) = left_select { self.left.tabs[left_ai].list_state.selected = Some(p); }
                }
                if let Some(p) = left_nav {
                    let h = self.config.show_hidden;
                    self.left.tabs[self.left.active_tab].navigate(p.clone(), h);
                    if let Some(sel) = left_sel_nav {
                        self.left.tabs[left_ai].list_state.selected = Some(sel);
                    }
                    self.config.last_path = Some(p);
                    self.config.save();
                }
                if left_clear_tag {
                    self.left.tabs[left_ai].tag_filter = None;
                    self.left.tabs[left_ai].tag_search_results = None;
                }
                if let Some(p) = left_ql { self.do_quicklook(p); }
                if left_focus_clicked { self.focus = PaneSide::Left; }

                // ── Right pane ────────────────────────────────────────────────
                let right_focus = self.focus == PaneSide::Right;
                let right_drop_target = is_tab_dragging
                    && self.tab_drag.as_ref().map_or(false, |d| d.from == PaneSide::Left);

                // Collect into locals so right borrow ends before handle_file_actions
                let (right_tab_actions, right_file_actions, right_focus_clicked) = {
                    let right = self.right.as_mut().unwrap();
                    render_pane(ui, right_rect, right, right_focus, right_drop_target, false, "right", &self.global_tags, cut_path.as_ref(), has_clipboard)
                };

                let mut remove_right = false;
                for action in right_tab_actions {
                    match action {
                        tab_bar::TabBarAction::Switch(i) => { self.right.as_mut().unwrap().active_tab = i; self.focus = PaneSide::Right; }
                        tab_bar::TabBarAction::Close(i) => {
                            if self.right.as_mut().unwrap().close_tab(i) { remove_right = true; }
                        }
                        tab_bar::TabBarAction::New => { let h = self.config.show_hidden; self.right.as_mut().unwrap().new_tab(h); self.focus = PaneSide::Right; }
                        tab_bar::TabBarAction::DragTab(i) => { self.tab_drag = Some(TabDrag { from: PaneSide::Right, tab_idx: i }); }
                    }
                }
                let right_ai = self.right.as_ref().map(|r| r.active_tab).unwrap_or(0);
                let terminal = self.config.terminal;
                let (right_special, right_regular): (Vec<_>, Vec<_>) = right_file_actions.into_iter()
                    .partition(|a| matches!(a, FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) | FileListAction::StartRename(_) | FileListAction::RenameItem(_, _) | FileListAction::CopyFile(_) | FileListAction::CutFile(_) | FileListAction::PasteHere));
                let (right_creating, right_clipboard): (Vec<_>, Vec<_>) = right_special.into_iter()
                    .partition(|a| matches!(a, FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) | FileListAction::StartRename(_) | FileListAction::RenameItem(_, _)));
                let (right_nav, right_reload, right_ql, right_sel_nav, right_clear_tag) = Self::handle_file_actions(
                    right_regular,
                    &mut self.bookmarks,
                    &mut self.toasts,
                    terminal,
                    &mut self.right.as_mut().unwrap().tabs[right_ai].dragging_path,
                );
                let (right_create_reload, right_create_sel) = Self::handle_creating_actions(right_creating, self.right.as_mut().unwrap().tabs.get_mut(right_ai).unwrap(), &mut self.toasts);
                let right_paste_dir = self.right.as_ref().map(|r| r.tabs[right_ai].current_path.clone()).unwrap_or_default();
                let (right_clip_reload, right_clip_sel) = Self::handle_clipboard_actions(right_clipboard, &mut self.clipboard_op, &right_paste_dir, &mut self.toasts);
                let right_select = right_create_sel.or(right_clip_sel);
                if right_reload || right_create_reload || right_clip_reload {
                    let h = self.config.show_hidden;
                    if let Some(r) = &mut self.right {
                        r.tabs[right_ai].reload(h);
                        if let Some(p) = right_select { r.tabs[right_ai].list_state.selected = Some(p); }
                    }
                }
                if let Some(p) = right_nav {
                    let h = self.config.show_hidden;
                    if let Some(r) = &mut self.right {
                        r.tabs[right_ai].navigate(p, h);
                        if let Some(sel) = right_sel_nav {
                            r.tabs[right_ai].list_state.selected = Some(sel);
                        }
                    }
                }
                if right_clear_tag {
                    if let Some(r) = &mut self.right {
                        r.tabs[right_ai].tag_filter = None;
                        r.tabs[right_ai].tag_search_results = None;
                    }
                }
                if let Some(p) = right_ql { self.do_quicklook(p); }
                if right_focus_clicked { self.focus = PaneSide::Right; }
                if remove_right { self.right = None; self.focus = PaneSide::Left; }

            } else {
                // ── Only left pane (full rect) ────────────────────────────────
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
                let (left_tab_actions, left_file_actions, _) =
                    render_pane(ui, full_rect, &mut self.left, true, false, true, "left", &self.global_tags, cut_path.as_ref(), has_clipboard);

                for action in left_tab_actions {
                    match action {
                        tab_bar::TabBarAction::Switch(i) => { self.left.active_tab = i; }
                        tab_bar::TabBarAction::Close(i) => { self.left.close_tab(i); }
                        tab_bar::TabBarAction::New => { let h = self.config.show_hidden; self.left.new_tab(h); }
                        tab_bar::TabBarAction::DragTab(i) => { self.tab_drag = Some(TabDrag { from: PaneSide::Left, tab_idx: i }); }
                    }
                }
                let left_ai = self.left.active_tab;
                let terminal = self.config.terminal;
                let (left_special, left_regular): (Vec<_>, Vec<_>) = left_file_actions.into_iter()
                    .partition(|a| matches!(a, FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) | FileListAction::StartRename(_) | FileListAction::RenameItem(_, _) | FileListAction::CopyFile(_) | FileListAction::CutFile(_) | FileListAction::PasteHere));
                let (left_creating, left_clipboard): (Vec<_>, Vec<_>) = left_special.into_iter()
                    .partition(|a| matches!(a, FileListAction::StartCreating(_) | FileListAction::CreateItem(_, _) | FileListAction::StartRename(_) | FileListAction::RenameItem(_, _)));
                let (left_nav, left_reload, left_ql, left_sel_nav, left_clear_tag) = Self::handle_file_actions(
                    left_regular,
                    &mut self.bookmarks,
                    &mut self.toasts,
                    terminal,
                    &mut self.left.tabs[left_ai].dragging_path,
                );
                let (left_create_reload, left_create_sel) = Self::handle_creating_actions(left_creating, &mut self.left.tabs[left_ai], &mut self.toasts);
                let left_paste_dir = self.left.tabs[left_ai].current_path.clone();
                let (left_clip_reload, left_clip_sel) = Self::handle_clipboard_actions(left_clipboard, &mut self.clipboard_op, &left_paste_dir, &mut self.toasts);
                let left_select = left_create_sel.or(left_clip_sel);
                if left_reload || left_create_reload || left_clip_reload {
                    let h = self.config.show_hidden;
                    self.left.tabs[left_ai].reload(h);
                    if let Some(p) = left_select { self.left.tabs[left_ai].list_state.selected = Some(p); }
                }
                if let Some(p) = left_nav {
                    let h = self.config.show_hidden;
                    self.left.tabs[left_ai].navigate(p.clone(), h);
                    if let Some(sel) = left_sel_nav {
                        self.left.tabs[left_ai].list_state.selected = Some(sel);
                    }
                    self.config.last_path = Some(p);
                    self.config.save();
                }
                if left_clear_tag {
                    self.left.tabs[left_ai].tag_filter = None;
                    self.left.tabs[left_ai].tag_search_results = None;
                }
                if let Some(p) = left_ql { self.do_quicklook(p); }
            }

            // ── Git panel (right position, carved out of CentralPanel) ─────────
            if self.git_panel_open && self.config.git_panel_right {
                if let Some(ref wd) = self.git_workdir.clone() {
                    let gpr = egui::Rect::from_min_max(
                        egui::pos2(full_rect.max.x + 1.0, total_rect.min.y),
                        total_rect.max,
                    );
                    ui.painter().rect_filled(gpr, egui::CornerRadius::ZERO, ui.visuals().panel_fill);
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
                        self.config.git_panel_width =
                            (self.config.git_panel_width - div_resp.drag_delta().x).clamp(200.0, 700.0);
                    }
                    if div_resp.drag_stopped() { self.config.save(); }
                    if div_resp.hovered() || div_resp.dragged() {
                        ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }
                    ui.allocate_new_ui(
                        egui::UiBuilder::new().max_rect(gpr).id_salt("git_panel_right"),
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

        // ── Cross-pane drag-to-move ───────────────────────────────────────────
        let mut cross_pane_move: Option<(PathBuf, PathBuf)> = None;
        if pointer_released {
            if self.right.is_some() {
                if let Some(pos) = ctx.pointer_hover_pos() {
                    let half: f32 = 3.0;
                    let gap: f32 = 6.0;
                    let div_x = self.content_rect.left() + self.content_rect.width() * self.split_ratio;
                    let right_x = div_x + half + gap;
                    let left_x = div_x - half - gap;

                    if let Some(from) = self.left.active().dragging_path.clone() {
                        if pos.x > right_x {
                            let to_dir = self.right.as_ref().unwrap().active().current_path.clone();
                            cross_pane_move = Some((from, to_dir));
                        }
                    }
                    if cross_pane_move.is_none() {
                        if let Some(from) = self.right.as_ref().unwrap().active().dragging_path.clone() {
                            if pos.x < left_x {
                                let to_dir = self.left.active().current_path.clone();
                                cross_pane_move = Some((from, to_dir));
                            }
                        }
                    }
                }
            }
        }
        if let Some((from, to_dir)) = cross_pane_move {
            match move_path(&from, &to_dir) {
                Ok(_) => {
                    let name = from.file_name().unwrap_or_default().to_string_lossy().to_string();
                    self.toasts.push(format!("Moved: {}", name));
                    let h = self.config.show_hidden;
                    self.left.active_mut().reload(h);
                    if let Some(r) = &mut self.right { r.active_mut().reload(h); }
                }
                Err(e) => { self.toasts.push(format!("Move failed: {}", e)); }
            }
        }

        // ── External drag: trigger when cursor leaves the window ──────────────
        #[cfg(target_os = "macos")]
        if let Some(dragging_path) = self.left.active().dragging_path.clone()
            .or_else(|| self.right.as_ref().and_then(|r| r.active().dragging_path.clone()))
        {
            let window_rect = ctx.screen_rect();
            let cursor_left = ctx.input(|i| {
                i.pointer.hover_pos()
                    .map_or(false, |p| !window_rect.contains(p))
            });
            if cursor_left {
                crate::platform::drag::begin_external_drag(&[dragging_path.as_path()]);
                self.left.active_mut().dragging_path = None;
                if let Some(r) = &mut self.right { r.active_mut().dragging_path = None; }
            }
        }

        // ── Drop from Finder into the focused pane ────────────────────────────
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            let dest_dir = self.focused_pane().active().current_path.clone();
            let h = self.config.show_hidden;
            for file in &dropped {
                if let Some(src) = &file.path {
                    if let Some(name) = src.file_name() {
                        let dest = dest_dir.join(name);
                        if src.is_dir() {
                            // Ignore partial-failure: copy_path_recursive creates the
                            // destination directory via create_dir_all before recursing,
                            // so even a failed copy leaves a (possibly empty) directory
                            // on disk. Always reload so the listing reflects reality.
                            let _ = copy_path_recursive(src, &dest);
                        } else {
                            let _ = std::fs::copy(src, &dest);
                        }
                    }
                }
            }
            self.focused_pane_mut().active_mut().reload(h);
        }

        // ── Clear file drag state ─────────────────────────────────────────────
        if pointer_released {
            self.left.active_mut().dragging_path = None;
            if let Some(r) = &mut self.right { r.active_mut().dragging_path = None; }
        }
    }
}

/// Render one pane (tab bar + file list) into a rect within the given `ui`.
/// Returns (tab_bar_actions, file_list_actions, focus_clicked).
fn render_pane(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    pane: &mut PaneState,
    is_focused: bool,
    is_drop_target: bool,
    is_only_pane: bool,
    pane_id: &str,
    global_tags: &crate::core::global_tags::GlobalTags,
    cut_path: Option<&PathBuf>,
    has_clipboard: bool,
) -> (Vec<tab_bar::TabBarAction>, Vec<FileListAction>, bool) {
    let mut tab_actions = Vec::new();
    let mut file_actions = Vec::new();
    let mut focus_clicked = false;

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
    let bar_w = if is_focused && !is_only_pane { 3.0_f32 } else { 0.0_f32 };
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + bar_w, rect.min.y),
        rect.max,
    );

    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect).id_salt(pane_id), |ui| {
        ui.set_clip_rect(content_rect);

        // Tab bar
        let names = pane.tab_names();
        let (tb_actions, _) = tab_bar::show(ui, &names, pane.active_tab, is_drop_target);
        tab_actions = tb_actions;

        ui.separator();

        // File list
        let ai = pane.active_tab;
        {
            let t = &mut pane.tabs[ai];
            sort_entries(&mut t.entries, t.list_state.sort_col, t.list_state.sort_order);
        }
        let actions = {
            let t = &mut pane.tabs[ai];
            file_list::show(
                ui, &t.entries, &mut t.list_state, &t.current_path,
                t.tag_filter.as_deref(), global_tags,
                cut_path, has_clipboard, t.dragging_path.as_ref(),
                t.tag_search_results.as_deref(),
            )
        };
        file_actions = actions;

        // Detect click to focus without creating an overlapping interactive widget
        // (which would compete with file-list row interactions and swallow clicks).
        focus_clicked = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer.press_origin().map_or(false, |pos| rect.contains(pos))
        });
    });

    (tab_actions, file_actions, focus_clicked)
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

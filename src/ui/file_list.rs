use crate::core::display_text;
use crate::core::fs::{FileEntry, FileKind, SortColumn, SortOrder};
use crate::ui::text;
use eframe::egui;
use std::path::PathBuf;

pub enum FileListAction {
    Navigate(PathBuf),
    OpenFile(PathBuf),
    CopyPath(PathBuf),
    AddBookmark(PathBuf),
    RevealInFinder(PathBuf),
    OpenInTerminal(PathBuf),
    DragStarted(Vec<PathBuf>),
    QuickLook(PathBuf),
    Share(PathBuf),
    SetTags(PathBuf, Vec<crate::core::tags::Tag>),
    GetInfo(PathBuf),
    StartCreating(CreateKind),
    CreateItem(CreateKind, String),
    StartRename(PathBuf),
    RenameItem(PathBuf, String), // (old_path, new_name)
    CopyFiles(Vec<PathBuf>),
    CutFiles(Vec<PathBuf>),
    PasteHere,
    MoveItems(Vec<PathBuf>, PathBuf), // from_paths, to_dir
    DeleteFiles(Vec<PathBuf>),
    NavigateAndSelect(PathBuf, PathBuf), // dir_path, file_path
    ClearTagFilter,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CreateKind {
    File,
    Directory,
}

pub struct CreatingItem {
    pub kind: CreateKind,
    pub name: String,
    pub needs_focus: bool,
}

pub struct RenamingItem {
    pub path: PathBuf,
    pub name: String,
    pub needs_focus: bool,
}

pub struct FileListState {
    pub selected: Vec<PathBuf>,
    pub selection_anchor: Option<PathBuf>,
    pub sort_col: SortColumn,
    pub sort_order: SortOrder,
    pub creating: Option<CreatingItem>,
    pub renaming: Option<RenamingItem>,
}

impl Default for FileListState {
    fn default() -> Self {
        Self {
            selected: Vec::new(),
            selection_anchor: None,
            sort_col: SortColumn::Name,
            sort_order: SortOrder::Ascending,
            creating: None,
            renaming: None,
        }
    }
}

impl FileListState {
    pub fn primary_selection(&self) -> Option<&PathBuf> {
        self.selected.last()
    }

    pub fn select_only(&mut self, path: PathBuf) {
        self.selected.clear();
        self.selected.push(path.clone());
        self.selection_anchor = Some(path);
    }

    pub fn clear_selection(&mut self) {
        self.selected.clear();
        self.selection_anchor = None;
    }

    pub fn select_all<'a>(&mut self, paths: impl IntoIterator<Item = &'a PathBuf>) {
        self.selected = paths.into_iter().cloned().collect();
        self.selection_anchor = self.selected.last().cloned();
    }

    pub fn move_primary(&mut self, visible_paths: &[PathBuf], delta: isize, extend: bool) {
        if visible_paths.is_empty() {
            self.clear_selection();
            return;
        }
        let current = self
            .primary_selection()
            .and_then(|path| visible_paths.iter().position(|candidate| candidate == path));
        let next = match current {
            Some(index) => index
                .saturating_add_signed(delta)
                .min(visible_paths.len() - 1),
            None if delta < 0 => visible_paths.len() - 1,
            None => 0,
        };
        let next_path = visible_paths[next].clone();
        if extend {
            let anchor = self
                .selection_anchor
                .clone()
                .or_else(|| current.map(|index| visible_paths[index].clone()))
                .unwrap_or_else(|| next_path.clone());
            self.select_range(visible_paths, &anchor, &next_path, false);
            self.selection_anchor = Some(anchor);
        } else {
            self.select_only(next_path);
        }
    }

    fn update_from_click(
        &mut self,
        visible_paths: &[PathBuf],
        path: &PathBuf,
        command: bool,
        shift: bool,
    ) {
        if shift {
            let anchor = self
                .selection_anchor
                .clone()
                .unwrap_or_else(|| path.clone());
            self.select_range(visible_paths, &anchor, path, command);
            self.selection_anchor = Some(anchor);
        } else if command {
            if let Some(index) = self.selected.iter().position(|selected| selected == path) {
                self.selected.remove(index);
            } else {
                self.selected.push(path.clone());
            }
            self.selection_anchor = Some(path.clone());
        } else {
            self.select_only(path.clone());
        }
    }

    fn select_range(
        &mut self,
        visible_paths: &[PathBuf],
        anchor: &PathBuf,
        target: &PathBuf,
        additive: bool,
    ) {
        let Some(anchor_index) = visible_paths.iter().position(|path| path == anchor) else {
            self.select_only(target.clone());
            return;
        };
        let Some(target_index) = visible_paths.iter().position(|path| path == target) else {
            return;
        };
        if !additive {
            self.selected.clear();
        }
        let (start, end) = if anchor_index <= target_index {
            (anchor_index, target_index)
        } else {
            (target_index, anchor_index)
        };
        for path in &visible_paths[start..=end] {
            if !self.selected.contains(path) {
                self.selected.push(path.clone());
            }
        }
        if let Some(index) = self.selected.iter().position(|path| path == target) {
            let target = self.selected.remove(index);
            self.selected.push(target);
        }
    }
}

const ROW_HEIGHT: f32 = 22.0;

pub fn show(
    ui: &mut egui::Ui,
    entries: &[FileEntry],
    state: &mut FileListState,
    current_path: &PathBuf,
    tag_filter: Option<&str>,
    global_tags: &crate::core::global_tags::GlobalTags,
    cut_paths: &[PathBuf],
    has_clipboard: bool,
    dragging_paths: Option<&Vec<PathBuf>>,
    tag_search_results: Option<&[std::path::PathBuf]>,
) -> Vec<FileListAction> {
    let mut actions = Vec::new();

    // Breadcrumb bar. Each component is capped and elided, while the horizontal
    // ScrollArea keeps every component reachable in exceptionally deep paths.
    egui::ScrollArea::horizontal()
        .id_salt("file_list_breadcrumbs")
        .show(ui, |ui| {
    ui.horizontal(|ui| {
        let components: Vec<_> = current_path.iter().collect();
                let small_font = egui::TextStyle::Small.resolve(ui.style());
        let mut accumulated = PathBuf::new();
        for (i, component) in components.iter().enumerate() {
            accumulated.push(component);
                    let name = display_text::os_str(component);
                    let label = if i == 0 { "  /".to_string() } else { name };
                    let natural_width = ui.fonts(|fonts| {
                        fonts
                            .layout_no_wrap(label.clone(), small_font.clone(), egui::Color32::WHITE)
                            .size()
                            .x
                    }) + 14.0;
                    let response = ui
                        .add_sized(
                            egui::vec2(natural_width.clamp(28.0, 180.0), 20.0),
                            egui::Button::new(&label).small().truncate(),
                        )
                        .on_hover_text(display_text::path(&accumulated));
                    if response.clicked() {
                actions.push(FileListAction::Navigate(accumulated.clone()));
            }
            if i < components.len() - 1 {
                ui.label(egui::RichText::new(">").weak().small());
            }
        }
    });
        });

    ui.separator();

    // ── Global tag search results view ───────────────────────────────────────
    if let Some(results) = tag_search_results {
        let tag_name = tag_filter.unwrap_or("");
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                "  🏷  {} — {} file{}",
                    tag_name,
                    results.len(),
                if results.len() == 1 { "" } else { "s" },
                ))
                .small(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("✕")
                    .on_hover_text("Clear tag filter")
                    .clicked()
                {
                    actions.push(FileListAction::ClearTagFilter);
                }
            });
        });
        ui.separator();

        let avail_w = ui.available_width();
        let body_font = egui::TextStyle::Body.resolve(ui.style());
        let small_font = egui::TextStyle::Small.resolve(ui.style());

        egui::ScrollArea::vertical()
            .drag_to_scroll(false)
            .show(ui, |ui| {
            if results.is_empty() {
                let (row_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ROW_HEIGHT * 3.0),
                    egui::Sense::hover(),
                );
                ui.painter().text(
                    row_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "No files found with this tag",
                    body_font.clone(),
                    ui.visuals().weak_text_color(),
                );
            } else {
                for (i, path) in results.iter().enumerate() {
                    let is_dir = path.is_dir();
                        let name = display_text::file_name(path);
                        let icon = if is_dir { "📁" } else { file_icon(&name) };
                        let parent = path.parent().map(display_text::path).unwrap_or_default();
                    let is_selected = state.selected.contains(path);

                    let (row_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), ROW_HEIGHT),
                        egui::Sense::hover(),
                    );
                    let row_id = ui.id().with(("tsr", i));
                        let row_response = ui
                            .interact(row_rect, row_id, egui::Sense::click())
                            .on_hover_text(display_text::path(path));

                    if ui.is_rect_visible(row_rect) {
                        draw_row_bg(ui, row_rect, is_selected, row_response.hovered());
                        let text_color = if is_selected {
                            ui.visuals().selection.stroke.color
                        } else {
                            ui.visuals().text_color()
                        };
                            let split = avail_w * 0.45;
                            paint_cell(
                                ui,
                                cell_rect(row_rect, 0.0, split, 4.0, 4.0),
                                &format!("  {}  {}", icon, name),
                                &body_font,
                                text_color,
                            );
                            paint_cell(
                                ui,
                                cell_rect(row_rect, split, avail_w, 4.0, 4.0),
                                &parent,
                                &small_font,
                                ui.visuals().weak_text_color(),
                            );
                    }

                    if row_response.clicked() {
                        let modifiers = ui.input(|input| input.modifiers);
                            state.update_from_click(
                                results,
                                path,
                                modifiers.command,
                                modifiers.shift,
                            );
                    }
                    if row_response.double_clicked() {
                        if is_dir {
                            actions.push(FileListAction::Navigate(path.clone()));
                        } else {
                                let dir = path
                                    .parent()
                                .map(|p| p.to_path_buf())
                                .unwrap_or(path.clone());
                            actions.push(FileListAction::NavigateAndSelect(dir, path.clone()));
                        }
                    }
                }
            }

            // Empty space below results (clears selection on click)
            let cursor_top = ui.cursor().min.y;
            let clip_bottom = ui.clip_rect().max.y;
            let remaining = (clip_bottom - cursor_top).max(0.0);
            if remaining > 0.0 {
                let (_, bg_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), remaining),
                    egui::Sense::click(),
                );
                    if bg_resp.clicked() {
                        state.clear_selection();
                    }
            }
        });

        return actions;
    }

    // Column x offsets — proportional so all columns are always visible.
    // Name 48% | Size 12% | Kind 15% | Date Modified 25%
    let avail_w = ui.available_width();
    let col_x: [f32; 4] = [
        0.0,
        (avail_w * 0.48).floor(),
        (avail_w * 0.60).floor(),
        (avail_w * 0.75).floor(),
    ];

    // Column headers
    let header_cols: [(SortColumn, &str); 4] = [
        (SortColumn::Name,     "Name"),
        (SortColumn::Size,     "Size"),
        (SortColumn::Kind,     "Kind"),
        (SortColumn::Modified, "Date Modified"),
    ];

    let (header_rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, 18.0), egui::Sense::hover());
    let header_font = egui::FontId::proportional(11.0);
    for (i, (col, label)) in header_cols.iter().enumerate() {
        let is_active = state.sort_col == *col;
        let arrow = if is_active {
            if state.sort_order == SortOrder::Ascending {
                " ▲"
            } else {
                " ▼"
            }
        } else {
            ""
        };
        let col_w = if i + 1 < col_x.len() {
            col_x[i + 1] - col_x[i]
        } else {
            avail_w - col_x[i]
        };
        let col_rect = egui::Rect::from_min_size(
            egui::pos2(header_rect.left() + col_x[i], header_rect.top()),
            egui::vec2(col_w, 18.0),
        );
        paint_cell(
            ui,
            text::inset_horizontally(col_rect, 4.0, 4.0),
            &format!("{}{}", label, arrow),
            &header_font,
            ui.visuals().strong_text_color(),
        );
        if ui
            .interact(col_rect, ui.id().with(("hdr", i)), egui::Sense::click())
            .clicked()
        {
            if state.sort_col == *col {
                state.sort_order = if state.sort_order == SortOrder::Ascending {
                    SortOrder::Descending
                } else {
                    SortOrder::Ascending
                };
            } else {
                state.sort_col = *col;
                state.sort_order = SortOrder::Ascending;
            }
        }
    }

    ui.separator();

    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let small_font = egui::TextStyle::Small.resolve(ui.style());

    // Filter entries for display
    let display_entries: Vec<&FileEntry> = if let Some(filter) = tag_filter {
        entries
            .iter()
            .filter(|e| e.tags.iter().any(|t| t.name == filter))
            .collect()
    } else {
        entries.iter().collect()
    };
    let display_paths: Vec<PathBuf> = display_entries
        .iter()
        .map(|entry| entry.path.clone())
        .collect();

    egui::ScrollArea::vertical()
        .drag_to_scroll(false) // ScrollArea would steal drag events needed for DnD
        .show(ui, |ui| {
        // ── Inline creation row ──────────────────────────────────────────────
        if state.creating.is_some() {
            let (confirmed, cancelled, c_kind, c_name) = {
                let c = state.creating.as_mut().unwrap();
                let (row_rect, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), ROW_HEIGHT),
                    egui::Sense::hover(),
                );
                    let icon = if c.kind == CreateKind::Directory {
                        "📁"
                    } else {
                        "📄"
                    };
                if ui.is_rect_visible(row_rect) {
                    ui.painter().text(
                        egui::pos2(row_rect.left() + 4.0, row_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        format!("  {}  ", icon),
                        body_font.clone(),
                        ui.visuals().text_color(),
                    );
                }
                let text_left = row_rect.left() + 32.0;
                let text_rect = egui::Rect::from_min_max(
                    egui::pos2(text_left, row_rect.top() + 2.0),
                        egui::pos2(
                            (row_rect.left() + col_x[1] - 4.0).max(text_left),
                            row_rect.bottom() - 2.0,
                        ),
                );
                let resp = ui.put(text_rect, egui::TextEdit::singleline(&mut c.name));
                if c.needs_focus {
                    resp.request_focus();
                    c.needs_focus = false;
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let cancel =
                        resp.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Enter));
                (enter, cancel, c.kind, c.name.clone())
            };
            if confirmed && !c_name.trim().is_empty() {
                actions.push(FileListAction::CreateItem(c_kind, c_name));
                state.creating = None;
            } else if cancelled {
                state.creating = None;
            }
        }

        // Enter on selected file starts inline rename (only when no edit is already active)
        if state.renaming.is_none() && state.creating.is_none() {
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if state.selected.len() == 1 {
                    if let Some(sel) = state.primary_selection().cloned() {
                        actions.push(FileListAction::StartRename(sel));
                    }
                }
            }
        }

        // ".." up-directory row
        if let Some(parent) = current_path.parent() {
            let parent_path = parent.to_path_buf();
            let (row_rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_HEIGHT),
                egui::Sense::click(),
            );
            if ui.is_rect_visible(row_rect) {
                draw_row_bg(ui, row_rect, false, response.hovered());
                    paint_cell(
                        ui,
                        cell_rect(row_rect, col_x[0], col_x[1], 4.0, 4.0),
                        "  📁  ..",
                        &body_font,
                        ui.visuals().text_color(),
                    );
            }
            if response.double_clicked() {
                actions.push(FileListAction::Navigate(parent_path));
            }
        }

        let pointer_released = ui.input(|i| i.pointer.any_released());
        let is_file_dragging = dragging_paths.is_some();

        for (i, entry) in display_entries.iter().enumerate() {
            let is_selected = state.selected.contains(&entry.path);
            let is_cut = cut_paths.contains(&entry.path);
            let icon = match entry.kind {
                FileKind::Directory => "📁",
                FileKind::Symlink => "🔗",
                FileKind::File => file_icon(&entry.name),
            };

            // Allocate space first (advances the cursor), then interact with a
            // stable explicit ID so egui can track drag state across frames.
            // Auto-IDs from allocate_exact_size are unreliable inside ScrollAreas.
            let (row_rect, _) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_HEIGHT),
                egui::Sense::hover(),
            );
            let row_id = ui.id().with(("row", i));
                let row_response = ui
                    .interact(row_rect, row_id, egui::Sense::click_and_drag())
                    .on_hover_text(&entry.name);

            // Highlight directory as drop target when a file drag is in progress
            let is_dir_drop_target = is_file_dragging
                && entry.kind == FileKind::Directory
                && row_response.hovered()
                && dragging_paths.map_or(true, |paths| !paths.contains(&entry.path));

                let is_renaming = state
                    .renaming
                    .as_ref()
                    .map_or(false, |r| r.path == entry.path);
            // Carries (confirmed, cancelled, new_name) out of the visibility block.
            let mut rename_result: Option<(bool, bool, String)> = None;

            if ui.is_rect_visible(row_rect) {
                    draw_row_bg(
                        ui,
                        row_rect,
                        is_selected || is_dir_drop_target,
                        row_response.hovered(),
                    );

                if is_renaming {
                    // Draw icon first with painter, then put TextEdit on top.
                    // Order matters: draw_row_bg → painter icon → ui.put(TextEdit).
                    ui.painter().text(
                        egui::pos2(row_rect.left() + 4.0, row_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        format!("  {}  ", icon),
                        body_font.clone(),
                        ui.visuals().text_color(),
                    );
                    let text_rect = egui::Rect::from_min_max(
                        egui::pos2(row_rect.left() + 32.0, row_rect.top() + 2.0),
                            egui::pos2(
                                (row_rect.left() + col_x[1] - 4.0).max(row_rect.left() + 32.0),
                                row_rect.bottom() - 2.0,
                            ),
                    );
                    let resp = {
                        let r = state.renaming.as_mut().unwrap();
                        let resp = ui.put(text_rect, egui::TextEdit::singleline(&mut r.name));
                        if r.needs_focus {
                            resp.request_focus();
                            r.needs_focus = false;
                        }
                        resp
                    };
                        let confirmed =
                            resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    let cancelled = resp.lost_focus() && !confirmed;
                    let new_name = state.renaming.as_ref().unwrap().name.clone();
                    rename_result = Some((confirmed, cancelled, new_name));
                } else {
                    let text_color = if is_cut {
                        ui.visuals().weak_text_color()
                    } else if is_selected {
                        ui.visuals().selection.stroke.color
                    } else {
                        ui.visuals().text_color()
                    };
                    let small_color = if is_cut || !is_selected {
                        ui.visuals().weak_text_color()
                    } else {
                        text_color
                    };

                    let name_text = if is_cut {
                        format!("  {}  {} (cut)", icon, entry.name)
                    } else {
                        format!("  {}  {}", icon, entry.name)
                    };
                        let name_column = cell_rect(row_rect, col_x[0], col_x[1], 0.0, 0.0);
                        let chip_r = 4.0_f32;
                        let chip_spacing = chip_r * 2.0 + 3.0;
                        let visible_chip_count =
                            visible_tag_chip_count(name_column.width(), entry.tags.len());
                        let chip_total_width = if visible_chip_count == 0 {
                            0.0
                        } else {
                            visible_chip_count as f32 * chip_r * 2.0
                                + (visible_chip_count - 1) as f32 * 3.0
                        };
                        let chip_start_x = name_column.right() - chip_total_width - 6.0;
                        let name_right_padding = if visible_chip_count == 0 {
                            4.0
                        } else {
                            chip_total_width + 10.0
                        };

                        paint_cell(
                            ui,
                            text::inset_horizontally(name_column, 4.0, name_right_padding),
                            &name_text,
                            &body_font,
                            text_color,
                        );
                        paint_cell(
                            ui,
                            cell_rect(row_rect, col_x[1], col_x[2], 4.0, 4.0),
                            &entry.size_display(),
                            &small_font,
                            small_color,
                        );
                        paint_cell(
                            ui,
                            cell_rect(row_rect, col_x[2], col_x[3], 4.0, 4.0),
                            &entry.kind.to_string(),
                            &small_font,
                            small_color,
                        );
                        paint_cell(
                            ui,
                            cell_rect(row_rect, col_x[3], avail_w, 4.0, 4.0),
                            &entry.modified_display(),
                            &small_font,
                            small_color,
                        );

                        // Render only the chips that fit in the reserved part of
                        // the Name column, with a clipped painter as a final guard.
                        if visible_chip_count > 0 {
                            let chip_painter = ui.painter().with_clip_rect(name_column);
                        let cy = row_rect.center().y;
                            for (ci, tag) in entry.tags.iter().take(visible_chip_count).enumerate()
                            {
                                let cx = chip_start_x + ci as f32 * chip_spacing + chip_r;
                            let (r, g, b) = tag.color.rgb();
                                chip_painter.circle_filled(
                                egui::pos2(cx, cy),
                                chip_r,
                                egui::Color32::from_rgb(r, g, b),
                            );
                        }
                    }
                }
            }

            // Apply rename result after the visibility block (borrows on state are released).
            if let Some((confirmed, cancelled, new_name)) = rename_result {
                if confirmed {
                    if !new_name.trim().is_empty() {
                        actions.push(FileListAction::RenameItem(entry.path.clone(), new_name));
                    }
                    state.renaming = None;
                } else if cancelled {
                    state.renaming = None;
                }
            }

            if !is_renaming {
                if row_response.drag_started() {
                    if !state.selected.contains(&entry.path) {
                        state.select_only(entry.path.clone());
                    }
                    actions.push(FileListAction::DragStarted(state.selected.clone()));
                }

                // Drop dragged file onto a directory in the same pane
                if is_dir_drop_target && pointer_released {
                    if let Some(from) = dragging_paths {
                            actions
                                .push(FileListAction::MoveItems(from.clone(), entry.path.clone()));
                    }
                }

                if row_response.clicked() {
                    let modifiers = ui.input(|input| input.modifiers);
                    state.update_from_click(
                        &display_paths,
                        &entry.path,
                        modifiers.command,
                        modifiers.shift,
                    );
                    // Surrender keyboard focus so file-row clicks don't block Cmd+C/X/V shortcuts.
                    ui.memory_mut(|m| m.surrender_focus(row_id));
                }
                if row_response.double_clicked() {
                    match entry.kind {
                        FileKind::Directory => {
                            actions.push(FileListAction::Navigate(entry.path.clone()))
                        }
                        _ => actions.push(FileListAction::OpenFile(entry.path.clone())),
                    }
                }

                row_response.context_menu(|ui| {
                    let action_paths = if state.selected.contains(&entry.path) {
                        state.selected.clone()
                    } else {
                        vec![entry.path.clone()]
                    };
                        if ui
                            .button("Quick Look")
                            .on_hover_text("Preview (Space)")
                            .clicked()
                        {
                        actions.push(FileListAction::QuickLook(entry.path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Share…").clicked() {
                        actions.push(FileListAction::Share(entry.path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Rename").clicked() {
                        actions.push(FileListAction::StartRename(entry.path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Copy  ⌘C").clicked() {
                        actions.push(FileListAction::CopyFiles(action_paths.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Cut   ⌘X").clicked() {
                        actions.push(FileListAction::CutFiles(action_paths.clone()));
                        ui.close_menu();
                    }
                    if has_clipboard && ui.button("Paste ⌘V").clicked() {
                        actions.push(FileListAction::PasteHere);
                        ui.close_menu();
                    }
                    ui.separator();
                    ui.menu_button("Tags", |ui| {
                        if global_tags.items.is_empty() {
                            ui.label(
                                egui::RichText::new("No tags — create in sidebar")
                                    .small()
                                    .weak()
                                    .italics(),
                            );
                        } else {
                            for global_tag in &global_tags.items {
                                    let has_tag =
                                        entry.tags.iter().any(|t| t.name == global_tag.name);
                                let text = if has_tag {
                                    format!("✓  {}", global_tag.name)
                                } else {
                                    format!("    {}", global_tag.name)
                                };
                                let (r, g, b) = global_tag.rgb();
                                let dot_color = egui::Color32::from_rgb(r, g, b);
                                let btn = ui.button(text);
                                ui.painter().circle_filled(
                                    egui::pos2(btn.rect.left() + 8.0, btn.rect.center().y),
                                    4.0,
                                    dot_color,
                                );
                                if btn.clicked() {
                                    let mut new_tags = entry.tags.clone();
                                    if has_tag {
                                        new_tags.retain(|t| t.name != global_tag.name);
                                    } else {
                                        new_tags.push(crate::core::tags::Tag {
                                            name: global_tag.name.clone(),
                                            color: crate::core::tags::TagColor::from_number(
                                                global_tag.color,
                                            ),
                                        });
                                    }
                                    actions.push(FileListAction::SetTags(
                                        entry.path.clone(),
                                        new_tags,
                                    ));
                                    ui.close_menu();
                                }
                            }
                        }
                    });
                    ui.separator();
                    if ui.button("Copy Path").clicked() {
                        actions.push(FileListAction::CopyPath(entry.path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Add to Bookmarks").clicked() {
                        actions.push(FileListAction::AddBookmark(entry.path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Reveal in Finder").clicked() {
                        actions.push(FileListAction::RevealInFinder(entry.path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Open in Terminal").clicked() {
                        actions.push(FileListAction::OpenInTerminal(entry.path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Get Info").clicked() {
                        actions.push(FileListAction::GetInfo(entry.path.clone()));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Move to Trash  ⌘Delete").clicked() {
                        actions.push(FileListAction::DeleteFiles(action_paths));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("New File").clicked() {
                        actions.push(FileListAction::StartCreating(CreateKind::File));
                        ui.close_menu();
                    }
                    if ui.button("New Folder").clicked() {
                        actions.push(FileListAction::StartCreating(CreateKind::Directory));
                        ui.close_menu();
                    }
                });
            }
        }

        // Right-clickable empty space below the last file row.
        // Use the gap between the current layout cursor and the visible clip bottom.
        let cursor_top = ui.cursor().min.y;
        let clip_bottom = ui.clip_rect().max.y;
        let remaining = (clip_bottom - cursor_top).max(0.0);
        if remaining > 0.0 {
            let (_, bg_resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), remaining),
                egui::Sense::click(),
            );
            if bg_resp.clicked() {
                state.clear_selection();
            }
            bg_resp.context_menu(|ui| {
                if has_clipboard {
                    if ui.button("Paste ⌘V").clicked() {
                        actions.push(FileListAction::PasteHere);
                        ui.close_menu();
                    }
                    ui.separator();
                }
                if ui.button("New File").clicked() {
                    actions.push(FileListAction::StartCreating(CreateKind::File));
                    ui.close_menu();
                }
                if ui.button("New Folder").clicked() {
                    actions.push(FileListAction::StartCreating(CreateKind::Directory));
                    ui.close_menu();
                }
            });
        }
    });

    actions
}

fn cell_rect(
    row_rect: egui::Rect,
    start_offset: f32,
    end_offset: f32,
    left_padding: f32,
    right_padding: f32,
) -> egui::Rect {
    let start = start_offset.max(0.0).min(row_rect.width());
    let end = end_offset.max(start).min(row_rect.width());
    let raw = egui::Rect::from_min_max(
        egui::pos2(row_rect.left() + start, row_rect.top()),
        egui::pos2(row_rect.left() + end, row_rect.bottom()),
    );
    text::inset_horizontally(raw, left_padding, right_padding)
}

fn visible_tag_chip_count(name_column_width: f32, tag_count: usize) -> usize {
    const CHIP_DIAMETER: f32 = 8.0;
    const CHIP_SPACING: f32 = 3.0;
    const MAX_COLUMN_FRACTION: f32 = 0.35;

    let max_chip_width = name_column_width.max(0.0) * MAX_COLUMN_FRACTION;
    let max_visible = if max_chip_width >= CHIP_DIAMETER {
        ((max_chip_width + CHIP_SPACING) / (CHIP_DIAMETER + CHIP_SPACING)).floor() as usize
    } else {
        0
    };
    tag_count.min(max_visible)
}

/// Paint one elided line inside an exact column cell.
fn paint_cell(
    ui: &egui::Ui,
    cell_rect: egui::Rect,
    text: &str,
    font: &egui::FontId,
    color: egui::Color32,
) {
    text::paint_truncated(ui, cell_rect, text, font.clone(), color);
}

fn draw_row_bg(ui: &egui::Ui, rect: egui::Rect, selected: bool, hovered: bool) {
    let color = if selected {
        Some(ui.visuals().selection.bg_fill)
    } else if hovered {
        Some(ui.visuals().widgets.hovered.weak_bg_fill)
    } else {
        None
    };
    if let Some(c) = color {
        ui.painter().rect_filled(rect, 2.0, c);
    }
}

fn file_icon(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "rs" | "py" | "js" | "ts" | "go" | "c" | "cpp" | "h" | "swift" => "📄",
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" => "🖼",
        "mp4" | "mov" | "avi" | "mkv" => "🎬",
        "mp3" | "wav" | "flac" | "aac" => "🎵",
        "pdf" => "📕",
        "zip" | "tar" | "gz" | "bz2" | "xz" | "7z" => "🗜",
        "md" | "txt" | "log" => "📝",
        _ => "📄",
    }
}

#[cfg(test)]
mod tests {
    use super::{cell_rect, visible_tag_chip_count, FileListState};
    use std::path::PathBuf;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn command_click_toggles_without_clearing_other_rows() {
        let visible = paths(&["a", "b", "c"]);
        let mut state = FileListState::default();
        state.update_from_click(&visible, &visible[0], false, false);
        state.update_from_click(&visible, &visible[2], true, false);
        assert_eq!(state.selected, paths(&["a", "c"]));

        state.update_from_click(&visible, &visible[0], true, false);
        assert_eq!(state.selected, paths(&["c"]));
    }

    #[test]
    fn shift_click_selects_inclusive_range_and_tracks_primary() {
        let visible = paths(&["a", "b", "c", "d"]);
        let mut state = FileListState::default();
        state.update_from_click(&visible, &visible[1], false, false);
        state.update_from_click(&visible, &visible[3], false, true);

        assert_eq!(state.selected, paths(&["b", "c", "d"]));
        assert_eq!(state.primary_selection(), Some(&visible[3]));
        assert_eq!(state.selection_anchor, Some(visible[1].clone()));
    }

    #[test]
    fn command_shift_click_adds_range() {
        let visible = paths(&["a", "b", "c", "d"]);
        let mut state = FileListState::default();
        state.update_from_click(&visible, &visible[0], false, false);
        state.update_from_click(&visible, &visible[2], true, false);
        state.update_from_click(&visible, &visible[3], true, true);

        assert_eq!(state.selected, paths(&["a", "c", "d"]));
    }

    #[test]
    fn shift_arrow_extends_from_original_anchor() {
        let visible = paths(&["a", "b", "c", "d"]);
        let mut state = FileListState::default();
        state.select_only(visible[1].clone());
        state.move_primary(&visible, 1, true);
        state.move_primary(&visible, 1, true);

        assert_eq!(state.selected, paths(&["b", "c", "d"]));
        assert_eq!(state.selection_anchor, Some(visible[1].clone()));
    }

    #[test]
    fn cell_rect_stays_inside_row_at_narrow_widths() {
        let row = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(12.0, 22.0),
        );
        let cell = cell_rect(row, 8.0, 20.0, 4.0, 4.0);
        assert!(cell.width() >= 0.0);
        assert!(cell.left() >= row.left());
        assert!(cell.right() <= row.right());
    }

    #[test]
    fn tag_chips_use_only_a_bounded_part_of_name_column() {
        assert_eq!(visible_tag_chip_count(10.0, 7), 0);
        assert_eq!(visible_tag_chip_count(100.0, 7), 3);
        assert_eq!(visible_tag_chip_count(300.0, 7), 7);
        assert_eq!(visible_tag_chip_count(300.0, 2), 2);
    }
}

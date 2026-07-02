use std::path::PathBuf;
use eframe::egui;
use crate::core::fs::{FileEntry, FileKind, SortColumn, SortOrder};

pub enum FileListAction {
    Navigate(PathBuf),
    OpenFile(PathBuf),
    CopyPath(PathBuf),
    AddBookmark(PathBuf),
    RevealInFinder(PathBuf),
    OpenInTerminal(PathBuf),
    DragStarted(PathBuf),
    QuickLook(PathBuf),
    Share(PathBuf),
    SetTags(PathBuf, Vec<crate::core::tags::Tag>),
    GetInfo(PathBuf),
    StartCreating(CreateKind),
    CreateItem(CreateKind, String),
    StartRename(PathBuf),
    RenameItem(PathBuf, String), // (old_path, new_name)
    CopyFile(PathBuf),
    CutFile(PathBuf),
    PasteHere,
    MoveItem(PathBuf, PathBuf), // from_path, to_dir
    DeleteFile(PathBuf),
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
    pub selected: Option<PathBuf>,
    pub sort_col: SortColumn,
    pub sort_order: SortOrder,
    pub creating: Option<CreatingItem>,
    pub renaming: Option<RenamingItem>,
}

impl Default for FileListState {
    fn default() -> Self {
        Self {
            selected: None,
            sort_col: SortColumn::Name,
            sort_order: SortOrder::Ascending,
            creating: None,
            renaming: None,
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
    cut_path: Option<&PathBuf>,
    has_clipboard: bool,
    dragging_path: Option<&PathBuf>,
    tag_search_results: Option<&[std::path::PathBuf]>,
) -> Vec<FileListAction> {
    let mut actions = Vec::new();

    // Breadcrumb bar
    ui.horizontal(|ui| {
        let components: Vec<_> = current_path.iter().collect();
        let mut accumulated = PathBuf::new();
        for (i, component) in components.iter().enumerate() {
            accumulated.push(component);
            let name = component.to_string_lossy();
            let label = if i == 0 { "  /".to_string() } else { name.to_string() };
            if ui.small_button(&label).clicked() {
                actions.push(FileListAction::Navigate(accumulated.clone()));
            }
            if i < components.len() - 1 {
                ui.label(egui::RichText::new(">").weak().small());
            }
        }
    });

    ui.separator();

    // ── Global tag search results view ───────────────────────────────────────
    if let Some(results) = tag_search_results {
        let tag_name = tag_filter.unwrap_or("");
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!(
                "  🏷  {} — {} file{}",
                tag_name, results.len(),
                if results.len() == 1 { "" } else { "s" },
            )).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("✕").on_hover_text("Clear tag filter").clicked() {
                    actions.push(FileListAction::ClearTagFilter);
                }
            });
        });
        ui.separator();

        let avail_w = ui.available_width();
        let body_font = egui::TextStyle::Body.resolve(ui.style());
        let small_font = egui::TextStyle::Small.resolve(ui.style());

        egui::ScrollArea::vertical().drag_to_scroll(false).show(ui, |ui| {
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
                    let icon = if is_dir { "📁" } else {
                        file_icon(path.file_name().and_then(|n| n.to_str()).unwrap_or(""))
                    };
                    let name = path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let parent = path.parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_default();
                    let is_selected = state.selected.as_ref() == Some(path);

                    let (row_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), ROW_HEIGHT),
                        egui::Sense::hover(),
                    );
                    let row_id = ui.id().with(("tsr", i));
                    let row_response = ui.interact(row_rect, row_id, egui::Sense::click());

                    if ui.is_rect_visible(row_rect) {
                        draw_row_bg(ui, row_rect, is_selected, row_response.hovered());
                        let text_color = if is_selected {
                            ui.visuals().selection.stroke.color
                        } else {
                            ui.visuals().text_color()
                        };
                        paint_cell(ui, row_rect, 0.0,
                            &format!("  {}  {}", icon, name), &body_font, text_color);
                        paint_cell(ui, row_rect, avail_w * 0.45,
                            &parent, &small_font, ui.visuals().weak_text_color());
                    }

                    if row_response.clicked() {
                        state.selected = Some(path.clone());
                    }
                    if row_response.double_clicked() {
                        if is_dir {
                            actions.push(FileListAction::Navigate(path.clone()));
                        } else {
                            let dir = path.parent()
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
                if bg_resp.clicked() { state.selected = None; }
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

    let (header_rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 18.0),
        egui::Sense::hover(),
    );
    let header_font = egui::FontId::proportional(11.0);
    for (i, (col, label)) in header_cols.iter().enumerate() {
        let is_active = state.sort_col == *col;
        let arrow = if is_active {
            if state.sort_order == SortOrder::Ascending { " ▲" } else { " ▼" }
        } else {
            ""
        };
        ui.painter().text(
            egui::pos2(header_rect.left() + col_x[i] + 4.0, header_rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}{}", label, arrow),
            header_font.clone(),
            ui.visuals().strong_text_color(),
        );
        let col_w = if i + 1 < col_x.len() { col_x[i + 1] - col_x[i] } else { avail_w - col_x[i] };
        let col_rect = egui::Rect::from_min_size(
            egui::pos2(header_rect.left() + col_x[i], header_rect.top()),
            egui::vec2(col_w, 18.0),
        );
        if ui.interact(col_rect, ui.id().with(("hdr", i)), egui::Sense::click()).clicked() {
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
        entries.iter().filter(|e| e.tags.iter().any(|t| t.name == filter)).collect()
    } else {
        entries.iter().collect()
    };

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
                let icon = if c.kind == CreateKind::Directory { "📁" } else { "📄" };
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
                    egui::pos2(row_rect.right() - 4.0, row_rect.bottom() - 2.0),
                );
                let resp = ui.put(text_rect, egui::TextEdit::singleline(&mut c.name));
                if c.needs_focus {
                    resp.request_focus();
                    c.needs_focus = false;
                }
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let cancel = resp.lost_focus() && !ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                if let Some(sel) = state.selected.clone() {
                    actions.push(FileListAction::StartRename(sel));
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
                paint_cell(ui, row_rect, col_x[0], "  📁  ..", &body_font, ui.visuals().text_color());
            }
            if response.double_clicked() {
                actions.push(FileListAction::Navigate(parent_path));
            }
        }

        let pointer_released = ui.input(|i| i.pointer.any_released());
        let is_file_dragging = dragging_path.is_some();

        for (i, entry) in display_entries.iter().enumerate() {
            let is_selected = state.selected.as_ref().map(|p| p == &entry.path).unwrap_or(false);
            let is_cut = cut_path.map_or(false, |p| p == &entry.path);
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
            let row_response = ui.interact(row_rect, row_id, egui::Sense::click_and_drag());

            // Highlight directory as drop target when a file drag is in progress
            let is_dir_drop_target = is_file_dragging
                && entry.kind == FileKind::Directory
                && row_response.hovered()
                && dragging_path.map_or(true, |p| p != &entry.path);

            let is_renaming = state.renaming.as_ref().map_or(false, |r| r.path == entry.path);
            // Carries (confirmed, cancelled, new_name) out of the visibility block.
            let mut rename_result: Option<(bool, bool, String)> = None;

            if ui.is_rect_visible(row_rect) {
                draw_row_bg(ui, row_rect, is_selected || is_dir_drop_target, row_response.hovered());

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
                        egui::pos2(row_rect.right() - 4.0, row_rect.bottom() - 2.0),
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
                    let confirmed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                    paint_cell(ui, row_rect, col_x[0], &name_text, &body_font, text_color);
                    paint_cell(ui, row_rect, col_x[1], &entry.size_display(), &small_font, small_color);
                    paint_cell(ui, row_rect, col_x[2], &entry.kind.to_string(), &small_font, small_color);
                    paint_cell(ui, row_rect, col_x[3], &entry.modified_display(), &small_font, small_color);

                    // Render tag color chips
                    if !entry.tags.is_empty() {
                        let chip_r = 4.0_f32;
                        let chip_spacing = chip_r * 2.0 + 3.0;
                        let n = entry.tags.len() as f32;
                        let total_w = n * (chip_r * 2.0) + (n - 1.0) * 3.0;
                        let start_x = row_rect.left() + col_x[1] - total_w - 6.0;
                        let cy = row_rect.center().y;
                        for (ci, tag) in entry.tags.iter().enumerate() {
                            let cx = start_x + ci as f32 * chip_spacing + chip_r;
                            let (r, g, b) = tag.color.rgb();
                            ui.painter().circle_filled(
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
                    actions.push(FileListAction::DragStarted(entry.path.clone()));
                }

                // Drop dragged file onto a directory in the same pane
                if is_dir_drop_target && pointer_released {
                    if let Some(from) = dragging_path {
                        actions.push(FileListAction::MoveItem(from.clone(), entry.path.clone()));
                    }
                }

                if row_response.clicked() {
                    state.selected = Some(entry.path.clone());
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
                    if ui.button("Quick Look").on_hover_text("Preview (Space)").clicked() {
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
                        actions.push(FileListAction::CopyFile(entry.path.clone()));
                        ui.close_menu();
                    }
                    if ui.button("Cut   ⌘X").clicked() {
                        actions.push(FileListAction::CutFile(entry.path.clone()));
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
                                let has_tag = entry.tags.iter().any(|t| t.name == global_tag.name);
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
                        actions.push(FileListAction::DeleteFile(entry.path.clone()));
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
                state.selected = None;
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

/// Paint text in a column cell using the painter directly (no widget allocation).
fn paint_cell(
    ui: &egui::Ui,
    row_rect: egui::Rect,
    col_x_offset: f32,
    text: &str,
    font: &egui::FontId,
    color: egui::Color32,
) {
    let x = row_rect.left() + col_x_offset + 4.0;
    let y = row_rect.center().y;
    ui.painter().text(
        egui::pos2(x, y),
        egui::Align2::LEFT_CENTER,
        text,
        font.clone(),
        color,
    );
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

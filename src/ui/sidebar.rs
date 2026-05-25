use std::path::PathBuf;
use eframe::egui;
use crate::core::bookmarks::Bookmarks;
use crate::core::global_tags::GlobalTags;

pub enum SidebarAction {
    Navigate(PathBuf),
    OpenFile(PathBuf),
    AddBookmark(PathBuf),
    MoveFileTo(PathBuf, PathBuf), // from_path, to_dir (drop onto a directory bookmark)
    FilterTag(Option<String>),
    CreateTag(String, u8),
    DeleteTag(usize),
    StartEdit(usize),           // begin editing tag at idx
    CommitEdit(usize, String, u8), // save (idx, new_name, new_color)
    CancelEdit,
}

pub struct FavoriteLocation {
    pub name: &'static str,
    pub path: PathBuf,
}

pub fn favorite_locations() -> Vec<FavoriteLocation> {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));
    vec![
        FavoriteLocation { name: "Home", path: home.clone() },
        FavoriteLocation { name: "Desktop", path: home.join("Desktop") },
        FavoriteLocation { name: "Downloads", path: home.join("Downloads") },
        FavoriteLocation { name: "Documents", path: home.join("Documents") },
        FavoriteLocation { name: "Applications", path: PathBuf::from("/Applications") },
    ]
}

pub fn show(
    ui: &mut egui::Ui,
    bookmarks: &mut Bookmarks,
    global_tags: &GlobalTags,
    current_path: &PathBuf,
    dragging_path: &Option<PathBuf>,
    active_tag: Option<&str>,
    new_tag_input: &mut String,
    new_tag_color: &mut u8,
    edit_tag_idx: &mut Option<usize>,
    edit_tag_name: &mut String,
    edit_tag_color: &mut u8,
) -> Vec<SidebarAction> {
    let mut actions = Vec::new();
    let is_dragging = dragging_path.is_some();

    ui.add_space(4.0);
    ui.label(egui::RichText::new("FAVORITES").small().weak());
    ui.add_space(2.0);

    for fav in favorite_locations() {
        if !fav.path.exists() { continue; }
        let selected = current_path == &fav.path;
        if ui.add(egui::SelectableLabel::new(selected, format!("  {}", fav.name))).clicked() {
            actions.push(SidebarAction::Navigate(fav.path));
        }
    }

    ui.add_space(8.0);

    // ── Bookmarks — drop target ───────────────────────────────────────────────
    let section_top = ui.cursor().min.y;
    ui.label(egui::RichText::new("BOOKMARKS").small().weak());
    ui.add_space(2.0);

    let pointer_released = ui.input(|i| i.pointer.any_released());
    let pointer_pos = ui.input(|i| i.pointer.hover_pos());

    let mut remove_idx: Option<usize> = None;
    let mut bookmark_drop_handled = false;
    if bookmarks.items.is_empty() {
        ui.label(egui::RichText::new("  Drop folders here").small().weak().italics());
    }
    for (i, bookmark) in bookmarks.items.iter().enumerate() {
        let selected = current_path == &bookmark.path;
        let response = ui.add(egui::SelectableLabel::new(selected, format!("  {}", bookmark.name)));

        // Per-bookmark drop target: dragging a file onto a directory bookmark moves it there
        if is_dragging && bookmark.path.is_dir() && response.hovered() {
            ui.painter().rect_stroke(
                response.rect,
                2.0,
                egui::Stroke::new(1.5, ui.visuals().selection.bg_fill),
                egui::StrokeKind::Inside,
            );
            if pointer_released {
                if let Some(from) = dragging_path {
                    actions.push(SidebarAction::MoveFileTo(from.clone(), bookmark.path.clone()));
                    bookmark_drop_handled = true;
                }
            }
        }

        if response.clicked() {
            if bookmark.path.is_dir() {
                actions.push(SidebarAction::Navigate(bookmark.path.clone()));
            } else {
                actions.push(SidebarAction::OpenFile(bookmark.path.clone()));
            }
        }
        response.context_menu(|ui| {
            if ui.button("Remove Bookmark").clicked() {
                remove_idx = Some(i);
                ui.close_menu();
            }
        });
    }
    if let Some(idx) = remove_idx { bookmarks.remove(idx); }

    let section_bottom = ui.cursor().min.y;
    let section_rect = egui::Rect::from_min_max(
        egui::pos2(ui.clip_rect().left(), section_top),
        egui::pos2(ui.clip_rect().right(), section_bottom.max(section_top + 40.0)),
    );
    if is_dragging {
        ui.painter().rect_stroke(
            section_rect, 4.0,
            egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
            egui::StrokeKind::Inside,
        );
    }
    // Section-wide drop: add as bookmark (only if no specific bookmark row caught it)
    if is_dragging && !bookmark_drop_handled && pointer_released && pointer_pos.map_or(false, |p| section_rect.contains(p)) {
        if let Some(path) = dragging_path {
            actions.push(SidebarAction::AddBookmark(path.clone()));
        }
    }

    // ── Tags ──────────────────────────────────────────────────────────────────
    ui.add_space(8.0);
    ui.label(egui::RichText::new("TAGS").small().weak());
    ui.add_space(4.0);

    // New tag row: text input + "+" button
    ui.horizontal(|ui| {
        let text_resp = ui.add(
            egui::TextEdit::singleline(new_tag_input)
                .hint_text("New tag…")
                .desired_width(120.0),
        );
        let add_clicked = ui.button("+").on_hover_text("Add tag").clicked();
        let enter_pressed = text_resp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (add_clicked || enter_pressed) && !new_tag_input.trim().is_empty() {
            actions.push(SidebarAction::CreateTag(
                new_tag_input.trim().to_string(),
                *new_tag_color,
            ));
            new_tag_input.clear();
        }
    });

    // Color picker for new tag
    color_picker_row(ui, new_tag_color);
    ui.add_space(4.0);

    // Tag list
    if global_tags.items.is_empty() {
        ui.label(egui::RichText::new("  No tags yet").small().weak().italics());
    } else {
        let mut delete_idx: Option<usize> = None;
        let mut start_edit_idx: Option<usize> = None;

        for (i, tag) in global_tags.items.iter().enumerate() {
            if *edit_tag_idx == Some(i) {
                // ── Inline editor ─────────────────────────────────────────────
                color_picker_row(ui, edit_tag_color);
                let text_resp = ui.add(
                    egui::TextEdit::singleline(edit_tag_name)
                        .desired_width(ui.available_width() - 4.0),
                );
                // Request focus on the first frame the editor appears
                if text_resp.gained_focus() || !text_resp.has_focus() && text_resp.changed() {
                    // keep focus
                }
                text_resp.request_focus();

                let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
                let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                let lost = text_resp.lost_focus() && !enter && !escape;

                if escape {
                    actions.push(SidebarAction::CancelEdit);
                } else if enter || lost {
                    let name = edit_tag_name.trim().to_string();
                    if !name.is_empty() {
                        actions.push(SidebarAction::CommitEdit(i, name, *edit_tag_color));
                    } else {
                        actions.push(SidebarAction::CancelEdit);
                    }
                }
            } else {
                // ── Normal row ────────────────────────────────────────────────
                let is_active = active_tag == Some(tag.name.as_str());
                let (r, g, b) = tag.rgb();
                let dot_color = egui::Color32::from_rgb(r, g, b);

                let response = ui.add(egui::SelectableLabel::new(
                    is_active,
                    format!("    {}", tag.name),
                ));
                ui.painter().circle_filled(
                    egui::pos2(response.rect.left() + 8.0, response.rect.center().y),
                    4.0,
                    dot_color,
                );
                if response.clicked() {
                    let filter = if is_active { None } else { Some(tag.name.clone()) };
                    actions.push(SidebarAction::FilterTag(filter));
                }
                response.context_menu(|ui| {
                    if ui.button("Edit Tag").clicked() {
                        start_edit_idx = Some(i);
                        ui.close_menu();
                    }
                    if ui.button("Delete Tag").clicked() {
                        delete_idx = Some(i);
                        ui.close_menu();
                    }
                });
            }
        }

        if let Some(idx) = delete_idx { actions.push(SidebarAction::DeleteTag(idx)); }
        if let Some(idx) = start_edit_idx { actions.push(SidebarAction::StartEdit(idx)); }
    }

    actions
}

/// A horizontal row of 7 colored circles for picking a tag color.
/// Mutates `selected` directly when a circle is clicked.
fn color_picker_row(ui: &mut egui::Ui, selected: &mut u8) {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        for color_idx in 1u8..=7u8 {
            let (r, g, b) = crate::core::tags::TagColor::from_number(color_idx).rgb();
            let fill = egui::Color32::from_rgb(r, g, b);
            let (resp, painter) =
                ui.allocate_painter(egui::Vec2::splat(16.0), egui::Sense::click());
            let is_sel = *selected == color_idx;
            painter.circle_filled(resp.rect.center(), if is_sel { 6.0 } else { 4.0 }, fill);
            if is_sel {
                painter.circle_stroke(
                    resp.rect.center(),
                    7.5,
                    egui::Stroke::new(1.5, ui.visuals().text_color()),
                );
            }
            if resp.clicked() { *selected = color_idx; }
        }
    });
}

use std::path::{Path, PathBuf};
use eframe::egui;

pub enum SearchAction {
    /// Navigate to this path (go to parent dir and select the entry).
    Navigate(PathBuf),
    /// Dismiss the overlay.
    Close,
}

/// Draw the floating search overlay.
///
/// Returns an action if the user commits a selection or presses Esc.
/// `just_opened` should be `true` on the first frame to auto-focus the input.
pub fn show(
    ctx: &egui::Context,
    root: &Path,
    query: &mut String,
    results: &[PathBuf],
    selected: &mut usize,
    just_opened: bool,
) -> Option<SearchAction> {
    // ── Keyboard handling (read before any widget consumes the events) ────────
    let pressed_esc   = ctx.input(|i| i.key_pressed(egui::Key::Escape));
    let pressed_enter = ctx.input(|i| i.key_pressed(egui::Key::Enter));
    let pressed_up    = ctx.input(|i| i.key_pressed(egui::Key::ArrowUp));
    let pressed_down  = ctx.input(|i| i.key_pressed(egui::Key::ArrowDown));

    if pressed_esc {
        return Some(SearchAction::Close);
    }
    if pressed_up && *selected > 0 {
        *selected -= 1;
    }
    if pressed_down && !results.is_empty() && *selected + 1 < results.len() {
        *selected += 1;
    }
    // Clamp in case results shrank
    if !results.is_empty() && *selected >= results.len() {
        *selected = results.len() - 1;
    }
    if pressed_enter {
        if let Some(path) = results.get(*selected) {
            return Some(SearchAction::Navigate(path.clone()));
        }
    }

    let screen = ctx.screen_rect();
    let overlay_w = (screen.width() * 0.55).clamp(380.0, 580.0);

    let mut committed: Option<SearchAction> = None;

    egui::Window::new("__search__")
        .title_bar(false)
        .resizable(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 60.0))
        .min_width(overlay_w)
        .max_width(overlay_w)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.set_min_width(overlay_w - 16.0);

            // ── Search input ───────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.label("🔍");
                let resp = ui.add(
                    egui::TextEdit::singleline(query)
                        .hint_text("Search in current folder…")
                        .desired_width(f32::INFINITY)
                        .frame(false),
                );
                if just_opened {
                    resp.request_focus();
                }
                if resp.changed() {
                    *selected = 0;
                }
            });

            // ── Results list ───────────────────────────────────────────────────
            if results.is_empty() {
                if !query.trim().is_empty() {
                    ui.separator();
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("No results")
                                .weak()
                                .italics()
                                .small(),
                        );
                    });
                    ui.add_space(6.0);
                }
                return;
            }

            ui.separator();

            let row_h = 44.0;
            let visible = results.len().min(12);
            let list_h = visible as f32 * row_h;

            egui::ScrollArea::vertical()
                .max_height(list_h)
                .show(ui, |ui| {
                    for (i, path) in results.iter().enumerate() {
                        let is_sel = *selected == i;

                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| path.to_string_lossy().into_owned());
                        let rel = relative_display(root, path);
                        let icon = if path.is_dir() { "📁" } else { "📄" };

                        let avail_w = ui.available_width();
                        let (rect, resp) = ui.allocate_exact_size(
                            egui::vec2(avail_w, row_h),
                            egui::Sense::click(),
                        );

                        if resp.hovered() {
                            *selected = i;
                        }

                        // Row background
                        if is_sel {
                            ui.painter().rect_filled(
                                rect,
                                4.0,
                                ui.visuals().selection.bg_fill,
                            );
                        } else if resp.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                4.0,
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }

                        let text_col = if is_sel {
                            ui.visuals().selection.stroke.color
                        } else {
                            ui.visuals().text_color()
                        };

                        // Primary: icon + file name
                        ui.painter().text(
                            egui::pos2(rect.left() + 10.0, rect.top() + 8.0),
                            egui::Align2::LEFT_TOP,
                            format!("{} {}", icon, name),
                            egui::FontId::proportional(13.0),
                            text_col,
                        );

                        // Secondary: relative path (dimmer, smaller)
                        ui.painter().text(
                            egui::pos2(rect.left() + 10.0, rect.top() + 26.0),
                            egui::Align2::LEFT_TOP,
                            rel,
                            egui::FontId::proportional(11.0),
                            ui.visuals().weak_text_color(),
                        );

                        if resp.clicked() {
                            committed = Some(SearchAction::Navigate(path.clone()));
                        }
                    }
                });

            // Hint line
            ui.separator();
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("↑↓ navigate   ↵ open   esc dismiss")
                        .weak()
                        .small(),
                );
            });
        });

    committed
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| {
            let s = p.to_string_lossy();
            if s.is_empty() { ".".to_string() } else { s.into_owned() }
        })
        .unwrap_or_else(|_| path.to_string_lossy().into_owned())
}

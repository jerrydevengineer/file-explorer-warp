use eframe::egui;

const TAB_HEIGHT: f32 = 28.0;
const TAB_MIN_W: f32 = 60.0;
const TAB_MAX_W: f32 = 180.0;
const CLOSE_W: f32 = 18.0;

pub enum TabBarAction {
    Switch(usize),
    Close(usize),
    New,
    DragTab(usize), // tab drag started from this bar
}

/// `dragging_tab` – set to true when a tab is being dragged from *any* pane,
/// so this bar can display a drop-target highlight.
pub fn show(
    ui: &mut egui::Ui,
    tab_names: &[String],
    active: usize,
    dragging_tab: bool,
) -> (Vec<TabBarAction>, egui::Rect) {
    let mut actions = Vec::new();
    let bar_rect = ui.available_rect_before_wrap();

    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let weak_color = ui.visuals().weak_text_color();
    let active_fill = ui.visuals().window_fill;
    let inactive_fill = ui.visuals().faint_bg_color;
    let hover_fill = ui.visuals().widgets.hovered.weak_bg_fill;
    let selection_color = ui.visuals().selection.bg_fill;

    ui.horizontal(|ui| {
        for (i, name) in tab_names.iter().enumerate() {
            let is_active = i == active;
            let show_close = tab_names.len() > 1;

            // Measure tab text width
            let text_w = ui.fonts(|f| {
                f.layout_no_wrap(name.clone(), body_font.clone(), egui::Color32::WHITE)
                    .rect
                    .width()
            });
            let tab_w = (text_w + 16.0 + if show_close { CLOSE_W } else { 0.0 })
                .clamp(TAB_MIN_W, TAB_MAX_W);

            // Allocate the tab rect
            let (tab_rect, _) = ui.allocate_exact_size(
                egui::vec2(tab_w, TAB_HEIGHT),
                egui::Sense::hover(),
            );
            let tab_id = ui.id().with(("tab", i));
            let tab_resp = ui.interact(tab_rect, tab_id, egui::Sense::click_and_drag());

            // Background
            let fill = if is_active {
                active_fill
            } else if tab_resp.hovered() {
                hover_fill
            } else {
                inactive_fill
            };
            ui.painter().rect_filled(tab_rect, egui::CornerRadius::same(4), fill);

            // Active tab bottom border (highlight)
            if is_active {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(tab_rect.left(), tab_rect.bottom() - 2.0),
                        egui::vec2(tab_rect.width(), 2.0),
                    ),
                    egui::CornerRadius::ZERO,
                    selection_color,
                );
            }

            // Tab label text
            let text_x = tab_rect.left() + 8.0;
            let text_right = if show_close { tab_rect.right() - CLOSE_W - 2.0 } else { tab_rect.right() - 8.0 };
            let text_rect = egui::Rect::from_min_max(
                egui::pos2(text_x, tab_rect.top()),
                egui::pos2(text_right, tab_rect.bottom()),
            );
            // Clip tab label to available space
            let painter = ui.painter().with_clip_rect(text_rect);
            painter.text(
                egui::pos2(text_x, tab_rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                body_font.clone(),
                if is_active { text_color } else { weak_color },
            );

            // Close button
            if show_close {
                let close_center = egui::pos2(tab_rect.right() - CLOSE_W * 0.5, tab_rect.center().y);
                let close_rect = egui::Rect::from_center_size(close_center, egui::vec2(CLOSE_W, CLOSE_W));
                let close_id = ui.id().with(("tab_close", i));
                let close_resp = ui.interact(close_rect, close_id, egui::Sense::click());
                if close_resp.hovered() {
                    ui.painter()
                        .rect_filled(close_rect, egui::CornerRadius::same(3), hover_fill);
                }
                ui.painter().text(
                    close_center,
                    egui::Align2::CENTER_CENTER,
                    "×",
                    body_font.clone(),
                    text_color,
                );
                if close_resp.clicked() {
                    actions.push(TabBarAction::Close(i));
                }
            }

            if tab_resp.drag_started() {
                actions.push(TabBarAction::DragTab(i));
            }
            if tab_resp.clicked() && !is_active {
                actions.push(TabBarAction::Switch(i));
            }

            ui.add_space(2.0);
        }

        // New-tab button
        if ui.button("+").on_hover_text("New tab (⌘T)").clicked() {
            actions.push(TabBarAction::New);
        }
    });

    // Drop-target outline drawn over the bar
    if dragging_tab {
        let full_bar = egui::Rect::from_min_max(bar_rect.min, egui::pos2(bar_rect.max.x, bar_rect.min.y + TAB_HEIGHT));
        ui.painter().rect_stroke(
            full_bar,
            egui::CornerRadius::same(4),
            egui::Stroke::new(2.0, selection_color),
            egui::StrokeKind::Inside,
        );
    }

    (actions, bar_rect)
}

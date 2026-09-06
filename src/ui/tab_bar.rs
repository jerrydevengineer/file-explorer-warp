use crate::ui::text;
use eframe::egui;

const TAB_HEIGHT: f32 = 28.0;
const SCROLLBAR_HEIGHT: f32 = 12.0;
const NEW_TAB_CONTROL_W: f32 = 30.0;
const CONTROL_GAP: f32 = 2.0;
const TAB_MIN_W: f32 = 60.0;
const TAB_MAX_W: f32 = 180.0;
const CLOSE_W: f32 = 18.0;

pub const fn strip_height() -> f32 {
    TAB_HEIGHT + SCROLLBAR_HEIGHT
}

fn tab_width(text_width: f32, show_close: bool) -> f32 {
    (text_width + 16.0 + if show_close { CLOSE_W } else { 0.0 }).clamp(TAB_MIN_W, TAB_MAX_W)
}

fn strip_sections(rect: egui::Rect) -> (egui::Rect, egui::Rect) {
    let control_left = (rect.right() - NEW_TAB_CONTROL_W).max(rect.left());
    let tabs_right = (control_left - CONTROL_GAP).max(rect.left());
    let tabs = egui::Rect::from_min_max(rect.min, egui::pos2(tabs_right, rect.bottom()));
    let control =
        egui::Rect::from_min_max(egui::pos2(control_left, rect.top()), rect.right_bottom());
    (tabs, control)
}

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
    let available = ui.available_rect_before_wrap();
    let bar_rect = egui::Rect::from_min_size(
        available.min,
        egui::vec2(available.width(), strip_height().min(available.height())),
    );
    let (tabs_rect, new_tab_control_rect) = strip_sections(bar_rect);

    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let weak_color = ui.visuals().weak_text_color();
    let active_fill = ui.visuals().window_fill;
    let inactive_fill = ui.visuals().faint_bg_color;
    let hover_fill = ui.visuals().widgets.hovered.weak_bg_fill;
    let selection_color = ui.visuals().selection.bg_fill;
    let active_identity = (active, tab_names.len());
    let last_active_id = ui.id().with("last_active_browser_tab");
    let reveal_active =
        ui.data(|data| data.get_temp::<(usize, usize)>(last_active_id) != Some(active_identity));
    ui.data_mut(|data| data.insert_temp(last_active_id, active_identity));

    if tabs_rect.width() > 0.0 && tabs_rect.height() > 0.0 {
        ui.allocate_new_ui(
            egui::UiBuilder::new()
                .max_rect(tabs_rect)
                .id_salt("browser_tab_scroll_viewport"),
            |ui| {
                ui.set_clip_rect(tabs_rect);
                ui.style_mut().always_scroll_the_only_direction = true;
                egui::ScrollArea::horizontal()
                    .id_salt("browser_tab_scroll")
                    .auto_shrink([false, false])
                    .drag_to_scroll(false)
                    .scroll_bar_visibility(
                        egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded,
                    )
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            for (i, name) in tab_names.iter().enumerate() {
                                let is_active = i == active;
                                let show_close = tab_names.len() > 1;

                                // Measure tab text width
                                let text_w = ui.fonts(|f| {
                                    f.layout_no_wrap(
                                        name.clone(),
                                        body_font.clone(),
                                        egui::Color32::WHITE,
                                    )
                                    .rect
                                    .width()
                                });
                                let tab_w = tab_width(text_w, show_close);

                                // Allocate the tab rect
                                let (tab_rect, _) = ui.allocate_exact_size(
                                    egui::vec2(tab_w, TAB_HEIGHT),
                                    egui::Sense::hover(),
                                );
                                let tab_id = ui.id().with(("tab", i));
                                let tab_resp = ui
                                    .interact(tab_rect, tab_id, egui::Sense::click_and_drag())
                                    .on_hover_text(name);
                                if is_active && reveal_active {
                                    tab_resp.scroll_to_me(Some(egui::Align::Center));
                                }

                                // Background
                                let fill = if is_active {
                                    active_fill
                                } else if tab_resp.hovered() {
                                    hover_fill
                                } else {
                                    inactive_fill
                                };
                                ui.painter().rect_filled(
                                    tab_rect,
                                    egui::CornerRadius::same(4),
                                    fill,
                                );

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
                                let text_right = if show_close {
                                    tab_rect.right() - CLOSE_W - 2.0
                                } else {
                                    tab_rect.right() - 8.0
                                };
                                let text_rect = egui::Rect::from_min_max(
                                    egui::pos2(text_x, tab_rect.top()),
                                    egui::pos2(text_right, tab_rect.bottom()),
                                );
                                text::paint_truncated(
                                    ui,
                                    text_rect,
                                    name,
                                    body_font.clone(),
                                    if is_active { text_color } else { weak_color },
                                );

                                // Close button
                                if show_close {
                                    let close_center = egui::pos2(
                                        tab_rect.right() - CLOSE_W * 0.5,
                                        tab_rect.center().y,
                                    );
                                    let close_rect = egui::Rect::from_center_size(
                                        close_center,
                                        egui::vec2(CLOSE_W, CLOSE_W),
                                    );
                                    let close_id = ui.id().with(("tab_close", i));
                                    let close_resp =
                                        ui.interact(close_rect, close_id, egui::Sense::click());
                                    if close_resp.hovered() {
                                        ui.painter().rect_filled(
                                            close_rect,
                                            egui::CornerRadius::same(3),
                                            hover_fill,
                                        );
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
                        });
                    });
            },
        );
    }

    let new_button_rect = egui::Rect::from_center_size(
        egui::pos2(
            new_tab_control_rect.center().x,
            bar_rect.top() + TAB_HEIGHT * 0.5,
        ),
        egui::vec2(24.0, 24.0),
    )
    .intersect(new_tab_control_rect);
    if new_button_rect.is_positive()
        && ui
            .put(new_button_rect, egui::Button::new("+"))
            .on_hover_text("New tab (⌘T)")
            .clicked()
    {
        actions.push(TabBarAction::New);
    }

    // Drop-target outline drawn over the bar
    if dragging_tab {
        let full_bar = egui::Rect::from_min_max(
            bar_rect.min,
            egui::pos2(bar_rect.max.x, bar_rect.min.y + TAB_HEIGHT),
        );
        ui.painter().rect_stroke(
            full_bar,
            egui::CornerRadius::same(4),
            egui::Stroke::new(2.0, selection_color),
            egui::StrokeKind::Inside,
        );
    }

    (actions, bar_rect)
}

#[cfg(test)]
mod tests {
    use super::{strip_sections, tab_width, NEW_TAB_CONTROL_W, TAB_MAX_W, TAB_MIN_W};
    use eframe::egui;

    #[test]
    fn tab_width_is_bounded_and_reserves_close_button_space() {
        assert_eq!(tab_width(0.0, false), TAB_MIN_W);
        assert_eq!(tab_width(10_000.0, true), TAB_MAX_W);
        assert!(tab_width(80.0, true) > tab_width(80.0, false));
    }

    #[test]
    fn new_tab_control_remains_fixed_when_tab_content_overflows() {
        let strip = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 40.0));
        let (tabs, control) = strip_sections(strip);

        assert_eq!(control.width(), NEW_TAB_CONTROL_W);
        assert_eq!(control.right(), strip.right());
        assert!(tabs.right() < control.left());
        assert_eq!(tabs.height(), strip.height());
    }
}

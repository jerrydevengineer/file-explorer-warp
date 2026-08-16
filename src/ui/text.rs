use eframe::egui;

/// Paint one line of text inside an exact rectangle, adding an ellipsis when
/// the text is wider than the available space. The clip rectangle remains as
/// a final containment guarantee for unusual glyph metrics.
pub fn paint_truncated(
    ui: &egui::Ui,
    rect: egui::Rect,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) -> bool {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return !text.is_empty();
    }

    let galley = egui::WidgetText::from(egui::RichText::new(text).font(font.clone()).color(color))
        .into_galley(ui, Some(egui::TextWrapMode::Truncate), rect.width(), font);
    let elided = galley.elided;
    let position = egui::pos2(rect.left(), rect.center().y - galley.size().y * 0.5);
    ui.painter()
        .with_clip_rect(rect)
        .galley(position, galley, color);
    elided
}

pub fn inset_horizontally(rect: egui::Rect, left: f32, right: f32) -> egui::Rect {
    let min_x = (rect.left() + left).min(rect.right());
    let max_x = (rect.right() - right).max(min_x);
    egui::Rect::from_min_max(
        egui::pos2(min_x, rect.top()),
        egui::pos2(max_x, rect.bottom()),
    )
}

#[cfg(test)]
mod tests {
    use super::{inset_horizontally, paint_truncated};

    #[test]
    fn horizontal_inset_never_returns_negative_width() {
        let rect = eframe::egui::Rect::from_min_size(
            eframe::egui::Pos2::ZERO,
            eframe::egui::vec2(5.0, 20.0),
        );
        let inset = inset_horizontally(rect, 4.0, 4.0);
        assert!(inset.width() >= 0.0);
        assert!(inset.left() >= rect.left());
        assert!(inset.right() <= rect.right());
    }

    #[test]
    fn long_text_is_elided_but_short_text_is_not() {
        let context = eframe::egui::Context::default();
        let mut long_elided = false;
        let mut short_elided = true;
        let _ = context.run(eframe::egui::RawInput::default(), |context| {
            eframe::egui::CentralPanel::default().show(context, |ui| {
                let origin = ui.available_rect_before_wrap().min;
                long_elided = paint_truncated(
                    ui,
                    eframe::egui::Rect::from_min_size(origin, eframe::egui::vec2(24.0, 22.0)),
                    "a very long file name.txt",
                    eframe::egui::FontId::proportional(14.0),
                    eframe::egui::Color32::WHITE,
                );
                short_elided = paint_truncated(
                    ui,
                    eframe::egui::Rect::from_min_size(origin, eframe::egui::vec2(400.0, 22.0)),
                    "short.txt",
                    eframe::egui::FontId::proportional(14.0),
                    eframe::egui::Color32::WHITE,
                );
            });
        });
        assert!(long_elided);
        assert!(!short_elided);
    }
}

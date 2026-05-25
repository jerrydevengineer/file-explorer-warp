use egui::{Color32, CornerRadius, Pos2, Rect, Vec2};
use std::time::{Duration, Instant};

const TOAST_WIDTH: f32 = 320.0;
const TOAST_HEIGHT: f32 = 40.0;
const TOAST_MARGIN: f32 = 12.0;
const TOAST_DURATION: Duration = Duration::from_secs(3);
const FADE_DURATION: Duration = Duration::from_millis(300);

pub struct Toast {
    pub message: String,
    created_at: Instant,
}

impl Toast {
    fn age(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Alpha 0.0–1.0 (fade-in for first 300ms, fade-out for last 300ms).
    fn alpha(&self) -> f32 {
        let age = self.age();
        let fade_secs = FADE_DURATION.as_secs_f32();
        if age < FADE_DURATION {
            age.as_secs_f32() / fade_secs
        } else if age > TOAST_DURATION - FADE_DURATION {
            let remaining = (TOAST_DURATION - age).as_secs_f32().max(0.0);
            remaining / fade_secs
        } else {
            1.0
        }
    }

    pub fn expired(&self) -> bool {
        self.age() >= TOAST_DURATION
    }
}

#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, message: impl Into<String>) {
        self.items.push(Toast {
            message: message.into(),
            created_at: Instant::now(),
        });
        // Keep at most 5 toasts visible
        if self.items.len() > 5 {
            self.items.remove(0);
        }
    }

    /// Draw all active toasts and remove expired ones. Call once per frame.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.items.retain(|t| !t.expired());

        if self.items.is_empty() {
            return;
        }

        // Request repaint while toasts are visible (for fade animation)
        ctx.request_repaint();

        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("toasts"),
        ));

        let font_id = egui::FontId::proportional(13.0);

        for (i, toast) in self.items.iter().enumerate() {
            let alpha = toast.alpha();
            let bottom_offset = TOAST_MARGIN + (TOAST_HEIGHT + TOAST_MARGIN) * i as f32;
            let rect = Rect::from_min_size(
                Pos2::new(
                    screen.center().x - TOAST_WIDTH / 2.0,
                    screen.max.y - bottom_offset - TOAST_HEIGHT,
                ),
                Vec2::new(TOAST_WIDTH, TOAST_HEIGHT),
            );

            let bg = Color32::from_rgba_unmultiplied(40, 40, 40, (220.0 * alpha) as u8);
            let fg = Color32::from_rgba_unmultiplied(255, 255, 255, (255.0 * alpha) as u8);

            painter.rect_filled(rect, CornerRadius::same(8), bg);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                &toast.message,
                font_id.clone(),
                fg,
            );
        }
    }
}

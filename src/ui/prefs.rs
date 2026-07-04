use egui::RichText;
use crate::core::config::{AppConfig, Theme, TerminalApp};
use crate::core::themes::CustomTheme;

pub struct PrefsAction {
    pub config_changed: bool,
}

pub fn show(ctx: &egui::Context, open: &mut bool, config: &mut AppConfig, themes: &[CustomTheme]) -> PrefsAction {
    let mut action = PrefsAction { config_changed: false };

    egui::Window::new("Preferences")
        .id(egui::Id::new("prefs_window"))
        .collapsible(false)
        .resizable(false)
        .default_width(400.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(open)
        .show(ctx, |ui| {
            ui.add_space(4.0);

            // ── Appearance ───────────────────────────────────────────────────
            ui.label(RichText::new("Appearance").strong());
            ui.separator();
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Theme:");
                ui.add_space(8.0);
                for (label, variant) in [("System", Theme::System), ("Light", Theme::Light), ("Dark", Theme::Dark)] {
                    let selected = config.theme == variant && config.custom_theme.is_none();
                    if ui.selectable_label(selected, label).clicked() {
                        if config.theme != variant || config.custom_theme.is_some() {
                            config.theme = variant;
                            config.custom_theme = None;
                            action.config_changed = true;
                        }
                    }
                }
            });

            if !themes.is_empty() {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    for theme in themes {
                        let accent = egui::Color32::from_rgb(
                            theme.colors.accent.r,
                            theme.colors.accent.g,
                            theme.colors.accent.b,
                        );
                        let is_selected = config.custom_theme.as_deref() == Some(theme.id.as_str());
                        let response = ui.selectable_label(is_selected, format!("   {}", theme.name));
                        let dot = egui::Pos2::new(
                            response.rect.min.x + 8.0,
                            response.rect.center().y,
                        );
                        ui.painter().circle_filled(dot, 5.0, accent);
                        if response.clicked() {
                            config.custom_theme = Some(theme.id.clone());
                            action.config_changed = true;
                        }
                    }
                });
            }

            ui.add_space(4.0);

            ui.horizontal(|ui| {
                let was = config.show_hidden;
                ui.checkbox(&mut config.show_hidden, "Show hidden files");
                if config.show_hidden != was {
                    action.config_changed = true;
                }
            });

            ui.add_space(12.0);

            // ── Terminal ─────────────────────────────────────────────────────
            ui.label(RichText::new("Terminal").strong());
            ui.separator();
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                ui.label("Open in terminal:");
                ui.add_space(8.0);
                for (label, variant) in [
                    ("Auto", TerminalApp::Auto),
                    ("Terminal.app", TerminalApp::Terminal),
                    ("iTerm2", TerminalApp::ITerm2),
                    ("Warp", TerminalApp::Warp),
                    ("Ghostty", TerminalApp::Ghostty),
                ] {
                    if ui.selectable_label(config.terminal == variant, label).clicked() {
                        if config.terminal != variant {
                            config.terminal = variant;
                            action.config_changed = true;
                        }
                    }
                }
            });

            ui.add_space(12.0);

            // ── Keyboard Shortcuts ───────────────────────────────────────────
            ui.label(RichText::new("Keyboard Shortcuts").strong());
            ui.separator();
            ui.add_space(4.0);

            egui::Grid::new("shortcuts_grid")
                .num_columns(2)
                .spacing([24.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    let shortcuts: &[(&str, &str)] = &[
                        ("⌘T",       "New tab"),
                        ("⌘W",       "Close tab"),
                        ("⌘N",       "New window"),
                        ("⌘1–9",     "Switch to tab N"),
                        ("⌘`",       "Switch pane focus"),
                        ("⌘\\",      "Close right pane"),
                        ("⌘R",       "Reload directory"),
                        ("⌘⇧.",      "Toggle hidden files"),
                        ("⌘F",       "Fuzzy search"),
                        ("⌘G",       "Toggle git panel"),
                        ("⌘,",       "Preferences"),
                        ("Backspace", "Go up one directory"),
                        ("Space",     "Quick Look preview"),
                        ("↑ / ↓",    "Select file"),
                        ("↵",        "Open / navigate into folder"),
                    ];
                    for (key, desc) in shortcuts {
                        ui.label(RichText::new(*key).monospace().strong());
                        ui.label(*desc);
                        ui.end_row();
                    }
                });

            ui.add_space(8.0);
        });

    action
}

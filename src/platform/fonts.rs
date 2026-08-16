use eframe::egui;

const TERMINAL_FAMILY_NAME: &str = "FileExplorerTerminal";
const TERMINAL_KOREAN_FONT_NAME: &str = "AppleGothicTerminal";

pub fn terminal_font_family() -> egui::FontFamily {
    egui::FontFamily::Name(TERMINAL_FAMILY_NAME.into())
}

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let embedded_monospace = fonts
        .families
        .get(&egui::FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    // macOS system fonts added as fallbacks after egui's embedded font.
    // egui tries each font in order; the first one with a glyph for a
    // given character wins. All paths are guaranteed present on macOS 12+.
    let candidates: &[(&str, &str)] = &[
        (
            "AppleSDGothicNeo",
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
        ),
        (
            "HiraginoSansGB",
            "/System/Library/Fonts/Hiragino Sans GB.ttc",
        ),
        ("SFArabic",         "/System/Library/Fonts/SFArabic.ttf"),
        ("SFHebrew",         "/System/Library/Fonts/SFHebrew.ttf"),
        (
            "ArialUnicode",
            "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        ),
    ];

    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert(name.to_string(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts
                    .families
                    .entry(family)
                    .or_default()
                    .push(name.to_string());
            }
        }
    }

    // Apple SD Gothic Neo is intentionally the general UI fallback, but its
    // 13 pt Hangul advance is much narrower than two cells of egui's embedded
    // monospace font. Use a terminal-only Korean face with a small visual
    // scale correction so adjacent wide glyphs read as a word instead of
    // appearing separated. The scale affects painting only, not cell layout.
    let mut terminal_fonts = embedded_monospace;
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/Supplemental/AppleGothic.ttf") {
        let terminal_korean = egui::FontData::from_owned(bytes).tweak(egui::FontTweak {
            scale: 1.08,
            ..Default::default()
        });
        fonts.font_data.insert(
            TERMINAL_KOREAN_FONT_NAME.to_string(),
            terminal_korean.into(),
        );
        terminal_fonts.push(TERMINAL_KOREAN_FONT_NAME.to_string());
    }
    for (name, _) in candidates {
        if fonts.font_data.contains_key(*name) {
            terminal_fonts.push(name.to_string());
        }
    }
    fonts
        .families
        .insert(terminal_font_family(), terminal_fonts);

    ctx.set_fonts(fonts);
}

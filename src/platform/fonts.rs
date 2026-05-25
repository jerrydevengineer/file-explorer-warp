use eframe::egui;

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();

    // macOS system fonts added as fallbacks after egui's embedded font.
    // egui tries each font in order; the first one with a glyph for a
    // given character wins. All paths are guaranteed present on macOS 12+.
    let candidates: &[(&str, &str)] = &[
        ("HiraginoSansGB",   "/System/Library/Fonts/Hiragino Sans GB.ttc"),
        ("AppleSDGothicNeo", "/System/Library/Fonts/AppleSDGothicNeo.ttc"),
        ("SFArabic",         "/System/Library/Fonts/SFArabic.ttf"),
        ("SFHebrew",         "/System/Library/Fonts/SFHebrew.ttf"),
        ("ArialUnicode",     "/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
    ];

    for (name, path) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                name.to_string(),
                egui::FontData::from_owned(bytes).into(),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families
                    .entry(family)
                    .or_default()
                    .push(name.to_string());
            }
        }
    }

    ctx.set_fonts(fonts);
}

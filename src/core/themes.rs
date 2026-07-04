use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ThemeColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeColors {
    pub background: ThemeColor,
    pub primary_text: ThemeColor,
    pub secondary_text: ThemeColor,
    pub accent: ThemeColor,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomTheme {
    pub id: String,
    pub name: String,
    pub description: String,
    pub colors: ThemeColors,
}

#[derive(Deserialize)]
struct ThemesFile {
    themes: Vec<CustomTheme>,
}

pub fn load_themes() -> Vec<CustomTheme> {
    let json = include_str!("../../themes.json");
    serde_json::from_str::<ThemesFile>(json)
        .map(|f| f.themes)
        .unwrap_or_default()
}

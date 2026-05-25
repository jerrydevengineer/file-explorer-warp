use std::path::Path;

const XATTR_KEY: &str = "com.apple.metadata:_kMDItemUserTags";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagColor {
    None,
    Gray,
    Green,
    Purple,
    Blue,
    Yellow,
    Red,
    Orange,
}

impl TagColor {
    pub fn from_number(n: u8) -> Self {
        match n {
            1 => TagColor::Gray,
            2 => TagColor::Green,
            3 => TagColor::Purple,
            4 => TagColor::Blue,
            5 => TagColor::Yellow,
            6 => TagColor::Red,
            7 => TagColor::Orange,
            _ => TagColor::None,
        }
    }

    fn to_number(self) -> u8 {
        match self {
            TagColor::None => 0,
            TagColor::Gray => 1,
            TagColor::Green => 2,
            TagColor::Purple => 3,
            TagColor::Blue => 4,
            TagColor::Yellow => 5,
            TagColor::Red => 6,
            TagColor::Orange => 7,
        }
    }

    /// Returns (r, g, b) for rendering — no egui dependency in core.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            TagColor::None | TagColor::Gray => (142, 142, 147),
            TagColor::Red => (255, 59, 48),
            TagColor::Orange => (255, 149, 0),
            TagColor::Yellow => (255, 204, 0),
            TagColor::Green => (52, 199, 89),
            TagColor::Blue => (0, 122, 255),
            TagColor::Purple => (175, 82, 222),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub color: TagColor,
}


pub fn read_tags(path: &Path) -> Vec<Tag> {
    let data = match xattr::get(path, XATTR_KEY) {
        Ok(Some(d)) => d,
        _ => return Vec::new(),
    };

    let tag_strings: Vec<String> = match plist::from_bytes(&data) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    tag_strings.iter().filter_map(|s| parse_tag_string(s)).collect()
}

pub fn write_tags(path: &Path, tags: &[Tag]) {
    if tags.is_empty() {
        let _ = xattr::remove(path, XATTR_KEY);
        return;
    }

    let tag_strings: Vec<String> = tags.iter().map(|t| {
        let n = t.color.to_number();
        if n == 0 {
            t.name.clone()
        } else {
            format!("{}\n{}", t.name, n)
        }
    }).collect();

    let mut buf = std::io::Cursor::new(Vec::new());
    if plist::to_writer_binary(&mut buf, &tag_strings).is_ok() {
        let _ = xattr::set(path, XATTR_KEY, buf.get_ref());
    }
}

fn parse_tag_string(s: &str) -> Option<Tag> {
    if s.is_empty() { return None; }
    let mut parts = s.splitn(2, '\n');
    let name = parts.next()?.to_string();
    if name.is_empty() { return None; }
    let color = parts.next()
        .and_then(|n| n.parse::<u8>().ok())
        .map(TagColor::from_number)
        .unwrap_or(TagColor::None);
    Some(Tag { name, color })
}

use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Vec2};
use crate::core::terminal::{TerminalState, TermColor, TerminalCell, TerminalGrid};

pub enum TerminalPanelEvent {
    OpenInTerminal,
}

const FONT_SIZE: f32 = 13.0;
const DEFAULT_FG: Color32 = Color32::from_rgb(204, 204, 204);
const DEFAULT_BG: Color32 = Color32::from_rgb(28, 28, 28);

// ── ANSI color palette ────────────────────────────────────────────────────────

fn ansi16(n: u8) -> Color32 {
    match n {
        0  => Color32::from_rgb(  0,   0,   0),
        1  => Color32::from_rgb(187,   0,   0),
        2  => Color32::from_rgb(  0, 187,   0),
        3  => Color32::from_rgb(187, 187,   0),
        4  => Color32::from_rgb(  0,   0, 187),
        5  => Color32::from_rgb(187,   0, 187),
        6  => Color32::from_rgb(  0, 187, 187),
        7  => Color32::from_rgb(187, 187, 187),
        8  => Color32::from_rgb( 85,  85,  85),
        9  => Color32::from_rgb(255,  85,  85),
        10 => Color32::from_rgb( 85, 255,  85),
        11 => Color32::from_rgb(255, 255,  85),
        12 => Color32::from_rgb( 85,  85, 255),
        13 => Color32::from_rgb(255,  85, 255),
        14 => Color32::from_rgb( 85, 255, 255),
        _  => Color32::from_rgb(255, 255, 255),
    }
}

fn indexed_color(n: u8) -> Color32 {
    if n < 16 {
        return ansi16(n);
    }
    if n < 232 {
        let v = n - 16;
        let bi = v % 6;
        let gi = (v / 6) % 6;
        let ri = v / 36;
        let ch = |c: u8| if c == 0 { 0u8 } else { c * 40 + 55 };
        return Color32::from_rgb(ch(ri), ch(gi), ch(bi));
    }
    let g = (n - 232) * 10 + 8;
    Color32::from_rgb(g, g, g)
}

fn resolve_fg(c: &TermColor, bold: bool) -> Color32 {
    match c {
        TermColor::Default => DEFAULT_FG,
        TermColor::Ansi(n) => {
            // Bold text in low 8 ANSI colors maps to bright variant (terminal convention)
            let n = if bold && *n < 8 { n + 8 } else { *n };
            ansi16(n)
        }
        TermColor::Indexed(n) => indexed_color(*n),
        TermColor::Rgb(r, g, b) => Color32::from_rgb(*r, *g, *b),
    }
}

fn resolve_bg(c: &TermColor) -> Color32 {
    match c {
        TermColor::Default => DEFAULT_BG,
        TermColor::Ansi(n) => ansi16(*n),
        TermColor::Indexed(n) => indexed_color(*n),
        TermColor::Rgb(r, g, b) => Color32::from_rgb(*r, *g, *b),
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn show(ui: &mut egui::Ui, state: &mut TerminalState) -> Option<TerminalPanelEvent> {
    let mut event: Option<TerminalPanelEvent> = None;
    let font_id = FontId::monospace(FONT_SIZE);

    // Measure a monospace cell once per frame
    let (char_w, char_h) = ui.fonts(|f| {
        (f.glyph_width(&font_id, 'M'), f.row_height(&font_id))
    });

    // ── Header bar ────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.add_space(4.0);

        let (title, cwd_str) = {
            let g = state.grid.lock().unwrap();
            let title = if !g.title.is_empty() {
                g.title.clone()
            } else {
                "zsh".to_string()
            };
            let cwd_str = g.cwd.as_ref()
                .and_then(|p| p.to_str())
                .unwrap_or("")
                .to_string();
            (title, cwd_str)
        };

        ui.label(egui::RichText::new(&title).small().strong());
        if !cwd_str.is_empty() {
            ui.label(egui::RichText::new(" — ").small().weak());
            ui.label(egui::RichText::new(&cwd_str).small().weak());
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("⌘J").small().weak());
            ui.separator();
            if ui.small_button("↗ Open in Terminal")
                .on_hover_text("Open current directory in external terminal (Warp/iTerm)")
                .clicked()
            {
                event = Some(TerminalPanelEvent::OpenInTerminal);
            }
        });
    });

    ui.separator();

    // ── Calculate grid dimensions ─────────────────────────────────────────────
    let avail = ui.available_size();
    let cols = ((avail.x / char_w).floor() as usize).max(1);
    let rows = ((avail.y / char_h).floor() as usize).max(1);

    {
        let g = state.grid.lock().unwrap();
        if g.cols != cols || g.rows != rows {
            drop(g);
            state.resize(cols, rows);
        }
    }

    // ── Allocate the grid rect ─────────────────────────────────────────────────
    let grid_size = Vec2::new(cols as f32 * char_w, rows as f32 * char_h);
    let (rect, response) = ui.allocate_exact_size(grid_size, Sense::click());

    if response.clicked() {
        response.request_focus();
    }
    let has_focus = response.has_focus();

    // ── Keyboard input ────────────────────────────────────────────────────────
    if has_focus {
        let mut to_send: Vec<u8> = Vec::new();

        ui.input_mut(|input| {
            input.events.retain(|event| {
                match event {
                    egui::Event::Text(text) => {
                        to_send.extend_from_slice(text.as_bytes());
                        false
                    }
                    egui::Event::Paste(text) => {
                        to_send.extend_from_slice(text.as_bytes());
                        false
                    }
                    egui::Event::Key { key, pressed: true, modifiers, .. } => {
                        // Ctrl+letter → control codes (only plain Ctrl, not Cmd)
                        if modifiers.ctrl && !modifiers.command && !modifiers.alt {
                            if let Some(code) = ctrl_seq(*key) {
                                to_send.push(code);
                                return false;
                            }
                        }
                        // Special keys → escape sequences
                        if let Some(seq) = special_seq(*key) {
                            to_send.extend_from_slice(&seq);
                            return false;
                        }
                        true
                    }
                    _ => true,
                }
            });
        });

        if !to_send.is_empty() {
            state.write_input(&to_send);
        }
    }

    // ── Scroll wheel → scrollback navigation ─────────────────────────────────
    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.5 {
            let lines = (scroll_y.abs() / char_h).ceil() as usize + 1;
            let mut g = state.grid.lock().unwrap();
            if scroll_y > 0.0 {
                g.scroll_offset = (g.scroll_offset + lines).min(g.scrollback.len());
            } else {
                g.scroll_offset = g.scroll_offset.saturating_sub(lines);
            }
        }
    }

    // ── Render ────────────────────────────────────────────────────────────────
    if !ui.is_rect_visible(rect) {
        return None;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, DEFAULT_BG);

    let grid = state.grid.lock().unwrap();
    let offset = grid.scroll_offset.min(grid.scrollback.len());

    // Background color pass (skip default-bg cells — already filled above)
    for disp_row in 0..rows {
        let Some(cells) = display_row(&grid, disp_row, offset) else { continue };
        let y = rect.min.y + disp_row as f32 * char_h;
        for col in 0..cols.min(cells.len()) {
            let bg = resolve_bg(&cells[col].bg);
            if bg != DEFAULT_BG {
                painter.rect_filled(
                    Rect::from_min_size(
                        Pos2::new(rect.min.x + col as f32 * char_w, y),
                        Vec2::new(char_w, char_h),
                    ),
                    0.0,
                    bg,
                );
            }
        }
    }

    // Character pass (skip space — background already correct)
    for disp_row in 0..rows {
        let Some(cells) = display_row(&grid, disp_row, offset) else { continue };
        let y = rect.min.y + disp_row as f32 * char_h;
        for col in 0..cols.min(cells.len()) {
            let cell = &cells[col];
            if cell.ch == ' ' {
                continue;
            }
            let fg = resolve_fg(&cell.fg, cell.bold);
            painter.text(
                Pos2::new(rect.min.x + col as f32 * char_w, y),
                egui::Align2::LEFT_TOP,
                cell.ch.to_string(),
                font_id.clone(),
                fg,
            );
        }
    }

    // ── Cursor ────────────────────────────────────────────────────────────────
    // Only show the cursor when not scrolled into history
    if offset == 0 && grid.cursor_row < rows && grid.cursor_col < cols {
        let cx = rect.min.x + grid.cursor_col as f32 * char_w;
        let cy = rect.min.y + grid.cursor_row as f32 * char_h;
        let cursor_rect = Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(char_w, char_h));

        if has_focus {
            // Solid block cursor, character inverted
            painter.rect_filled(cursor_rect, 0.0, DEFAULT_FG);
            if grid.cursor_row < grid.cells.len() {
                if let Some(cell) = grid.cells[grid.cursor_row].get(grid.cursor_col) {
                    if cell.ch != ' ' {
                        painter.text(
                            Pos2::new(cx, cy),
                            egui::Align2::LEFT_TOP,
                            cell.ch.to_string(),
                            font_id.clone(),
                            DEFAULT_BG,
                        );
                    }
                }
            }
        } else {
            // Hollow cursor when focus is elsewhere
            painter.rect_stroke(
                cursor_rect,
                0.0,
                egui::Stroke::new(1.0, DEFAULT_FG.gamma_multiply(0.5)),
                egui::StrokeKind::Inside,
            );
        }
    }

    // ── Scrollback indicator ──────────────────────────────────────────────────
    if offset > 0 && !grid.scrollback.is_empty() {
        let total = grid.scrollback.len() + grid.rows;
        let visible_frac = (grid.rows as f32 / total as f32).min(1.0);
        let scroll_frac = (offset as f32 / total as f32).min(1.0 - visible_frac);
        let bar_h = (rect.height() * visible_frac).max(8.0);
        let bar_y = rect.min.y + rect.height() * (1.0 - scroll_frac - visible_frac);
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(rect.max.x - 4.0, bar_y.max(rect.min.y)),
                Vec2::new(3.0, bar_h),
            ),
            1.0,
            DEFAULT_FG.gamma_multiply(0.35),
        );
    }

    drop(grid);
    event
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn display_row<'a>(
    grid: &'a TerminalGrid,
    disp_row: usize,
    offset: usize,
) -> Option<&'a Vec<TerminalCell>> {
    if offset == 0 {
        grid.cells.get(disp_row)
    } else if disp_row < offset {
        let idx = grid.scrollback.len().checked_sub(offset - disp_row)?;
        grid.scrollback.get(idx)
    } else {
        grid.cells.get(disp_row - offset)
    }
}

// Ctrl+letter → C0 control code (0x01–0x1A)
fn ctrl_seq(key: egui::Key) -> Option<u8> {
    match key {
        egui::Key::A => Some(0x01),
        egui::Key::B => Some(0x02),
        egui::Key::C => Some(0x03),
        egui::Key::D => Some(0x04),
        egui::Key::E => Some(0x05),
        egui::Key::F => Some(0x06),
        egui::Key::G => Some(0x07),
        egui::Key::H => Some(0x08),
        egui::Key::I => Some(0x09),
        egui::Key::J => Some(0x0a),
        egui::Key::K => Some(0x0b),
        egui::Key::L => Some(0x0c),
        egui::Key::M => Some(0x0d),
        egui::Key::N => Some(0x0e),
        egui::Key::O => Some(0x0f),
        egui::Key::P => Some(0x10),
        egui::Key::Q => Some(0x11),
        egui::Key::R => Some(0x12),
        egui::Key::S => Some(0x13),
        egui::Key::T => Some(0x14),
        egui::Key::U => Some(0x15),
        egui::Key::V => Some(0x16),
        egui::Key::W => Some(0x17),
        egui::Key::X => Some(0x18),
        egui::Key::Y => Some(0x19),
        egui::Key::Z => Some(0x1a),
        _ => None,
    }
}

// Special keys → VT100 escape sequences
fn special_seq(key: egui::Key) -> Option<Vec<u8>> {
    match key {
        egui::Key::Enter      => Some(b"\r".to_vec()),
        egui::Key::Backspace  => Some(vec![0x7f]),
        egui::Key::Tab        => Some(b"\t".to_vec()),
        egui::Key::Escape     => Some(vec![0x1b]),
        egui::Key::ArrowUp    => Some(b"\x1b[A".to_vec()),
        egui::Key::ArrowDown  => Some(b"\x1b[B".to_vec()),
        egui::Key::ArrowRight => Some(b"\x1b[C".to_vec()),
        egui::Key::ArrowLeft  => Some(b"\x1b[D".to_vec()),
        egui::Key::Home       => Some(b"\x1b[H".to_vec()),
        egui::Key::End        => Some(b"\x1b[F".to_vec()),
        egui::Key::PageUp     => Some(b"\x1b[5~".to_vec()),
        egui::Key::PageDown   => Some(b"\x1b[6~".to_vec()),
        egui::Key::Delete     => Some(b"\x1b[3~".to_vec()),
        egui::Key::Insert     => Some(b"\x1b[2~".to_vec()),
        egui::Key::F1         => Some(b"\x1bOP".to_vec()),
        egui::Key::F2         => Some(b"\x1bOQ".to_vec()),
        egui::Key::F3         => Some(b"\x1bOR".to_vec()),
        egui::Key::F4         => Some(b"\x1bOS".to_vec()),
        egui::Key::F5         => Some(b"\x1b[15~".to_vec()),
        egui::Key::F6         => Some(b"\x1b[17~".to_vec()),
        egui::Key::F7         => Some(b"\x1b[18~".to_vec()),
        egui::Key::F8         => Some(b"\x1b[19~".to_vec()),
        egui::Key::F9         => Some(b"\x1b[20~".to_vec()),
        egui::Key::F10        => Some(b"\x1b[21~".to_vec()),
        egui::Key::F11        => Some(b"\x1b[23~".to_vec()),
        egui::Key::F12        => Some(b"\x1b[24~".to_vec()),
        _ => None,
    }
}

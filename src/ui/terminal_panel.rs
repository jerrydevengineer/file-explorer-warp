use std::time::Duration;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Vec2};
use crate::core::terminal::{TerminalState, TermColor, TerminalCell, TerminalGrid};

pub enum TerminalPanelEvent {
    OpenInTerminal,
    NewTab,
    CloseTab(usize),
    SwitchTab(usize),
}

const FONT_SIZE: f32 = 13.0;
const DEFAULT_FG: Color32 = Color32::from_rgb(204, 204, 204);
const DEFAULT_BG: Color32 = Color32::from_rgb(28, 28, 28);
const SELECTION_BG: Color32 = Color32::from_rgb(48, 96, 160);

// ── Selection state ───────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct TermSelection {
    start: Option<(usize, usize)>,   // (display_row, col)
    end:   Option<(usize, usize)>,
}

impl TermSelection {
    fn is_active(&self) -> bool {
        matches!((self.start, self.end), (Some(s), Some(e)) if s != e)
    }

    fn normalized(&self) -> Option<((usize, usize), (usize, usize))> {
        let s = self.start?;
        let e = self.end?;
        if s.0 < e.0 || (s.0 == e.0 && s.1 <= e.1) { Some((s, e)) } else { Some((e, s)) }
    }

    fn contains(&self, row: usize, col: usize) -> bool {
        let Some(((sr, sc), (er, ec))) = self.normalized() else { return false };
        if row < sr || row > er { return false; }
        if row == sr && row == er { return col >= sc && col <= ec; }
        if row == sr { return col >= sc; }
        if row == er { return col <= ec; }
        true
    }
}

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

/// `terminals` is the full list of open sessions; `active` is the currently visible index.
pub fn show(
    ui: &mut egui::Ui,
    terminals: &mut [TerminalState],
    active: usize,
) -> Option<TerminalPanelEvent> {
    if terminals.is_empty() {
        return None;
    }

    let mut event: Option<TerminalPanelEvent> = None;
    let font_id = FontId::monospace(FONT_SIZE);

    // Measure a monospace cell once per frame
    let (char_w, char_h) = ui.fonts(|f| {
        (f.glyph_width(&font_id, 'M'), f.row_height(&font_id))
    });

    // ── Header: tab bar ───────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.add_space(4.0);

        // One button per tab; active tab is highlighted
        for (i, term) in terminals.iter().enumerate() {
            let title = {
                let g = term.grid.lock().unwrap();
                if g.title.is_empty() { "zsh".to_string() } else { g.title.clone() }
            };

            // Tab label — acts as a switch button
            let resp = ui.selectable_label(i == active, &title);
            if resp.clicked() && i != active && event.is_none() {
                event = Some(TerminalPanelEvent::SwitchTab(i));
            }

            // Close button only when there is more than one tab
            if terminals.len() > 1 {
                let close = ui.small_button("×").on_hover_text("Close tab");
                if close.clicked() && event.is_none() {
                    event = Some(TerminalPanelEvent::CloseTab(i));
                }
            }

            ui.add_space(2.0);
        }

        // New tab button
        if ui.small_button("+").on_hover_text("New terminal tab (opens in current directory)").clicked()
            && event.is_none()
        {
            event = Some(TerminalPanelEvent::NewTab);
        }

        // CWD of the active tab — shown after the tab list
        let cwd_str = {
            let g = terminals[active].grid.lock().unwrap();
            g.cwd.as_ref().and_then(|p| p.to_str()).unwrap_or("").to_string()
        };
        if !cwd_str.is_empty() {
            ui.separator();
            ui.label(egui::RichText::new(&cwd_str).small().weak());
        }

        // Right-aligned buttons
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("⌘J").small().weak());
            ui.separator();
            if ui.small_button("↗ Open in Terminal")
                .on_hover_text("Open current directory in external terminal (Warp/iTerm)")
                .clicked()
                && event.is_none()
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
        let g = terminals[active].grid.lock().unwrap();
        if g.cols != cols || g.rows != rows {
            drop(g);
            terminals[active].resize(cols, rows);
        }
    }

    // ── Allocate the grid rect ────────────────────────────────────────────────
    let grid_size = Vec2::new(cols as f32 * char_w, rows as f32 * char_h);
    // Use an explicit stable ID (not auto-generated) so focus survives layout
    // changes in the header (e.g. CWD label appearing after first shell output).
    let (rect, _) = ui.allocate_exact_size(grid_size, Sense::hover());
    let response = ui.interact(rect, ui.id().with("terminal_grid"), Sense::click_and_drag());

    // Focus on click or drag start
    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
    let has_focus = response.has_focus();

    // ── Cursor blink ──────────────────────────────────────────────────────────
    let time = ui.ctx().input(|i| i.time);
    let cursor_blink_on = !has_focus || ((time * 1000.0 / 600.0).floor() as u64 % 2 == 0);
    if has_focus {
        ui.ctx().request_repaint_after(Duration::from_millis(600));
    }

    // ── Selection state (per-tab via active index key) ────────────────────────
    let sel_id = egui::Id::new("terminal_selection").with(active);
    let mut sel: TermSelection = ui.data(|d| d.get_temp(sel_id).unwrap_or_default());

    if response.drag_started() {
        if let Some(pos) = response.interact_pointer_pos() {
            let (row, col) = pos_to_cell(pos, rect, char_w, char_h, rows, cols);
            sel = TermSelection { start: Some((row, col)), end: Some((row, col)) };
        }
    }
    if response.dragged() {
        if let Some(pos) = ui.ctx().pointer_hover_pos() {
            let clamped = Pos2::new(
                pos.x.clamp(rect.min.x, rect.max.x - 0.01),
                pos.y.clamp(rect.min.y, rect.max.y - 0.01),
            );
            let (row, col) = pos_to_cell(clamped, rect, char_w, char_h, rows, cols);
            sel.end = Some((row, col));
        }
    }
    if response.clicked() {
        sel = TermSelection::default();
    }

    // ── Keyboard input ────────────────────────────────────────────────────────
    if has_focus {
        // Lock all navigation keys to this widget so egui's internal focus
        // navigation doesn't consume them before our input_mut block runs.
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: true,
                },
            );
        });

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
                        if modifiers.ctrl && !modifiers.command && !modifiers.alt {
                            if let Some(code) = ctrl_seq(*key) {
                                to_send.push(code);
                                return false;
                            }
                        }
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
            sel = TermSelection::default();
            terminals[active].write_input(&to_send);
        }
    }

    // ── Scroll wheel → scrollback navigation ─────────────────────────────────
    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.5 {
            let lines = (scroll_y.abs() / char_h).ceil() as usize + 1;
            let mut g = terminals[active].grid.lock().unwrap();
            if scroll_y > 0.0 {
                g.scroll_offset = (g.scroll_offset + lines).min(g.scrollback.len());
            } else {
                g.scroll_offset = g.scroll_offset.saturating_sub(lines);
            }
        }
    }

    // Persist selection before potential early return
    ui.data_mut(|d| d.insert_temp(sel_id, sel.clone()));

    // ── Render ────────────────────────────────────────────────────────────────
    if !ui.is_rect_visible(rect) {
        return event;
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, DEFAULT_BG);

    let grid = terminals[active].grid.lock().unwrap();
    let offset = grid.scroll_offset.min(grid.scrollback.len());

    // ── Cmd+C: copy selected text ─────────────────────────────────────────────
    if has_focus && sel.is_active() {
        let copy_requested = ui.input_mut(|i| {
            if i.events.iter().any(|e| matches!(e, egui::Event::Copy)) {
                i.events.retain(|e| !matches!(e, egui::Event::Copy));
                true
            } else {
                false
            }
        });
        if copy_requested {
            let text = extract_selection_text(&grid, &sel, rows, offset);
            ui.ctx().copy_text(text);
        }
    }

    // Background color pass (selection highlight overrides cell bg)
    for disp_row in 0..rows {
        let Some(cells) = display_row(&grid, disp_row, offset) else { continue };
        let y = rect.min.y + disp_row as f32 * char_h;
        for col in 0..cols.min(cells.len()) {
            let bg = if sel.contains(disp_row, col) {
                SELECTION_BG
            } else {
                resolve_bg(&cells[col].bg)
            };
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

    // Character pass (selected cells use DEFAULT_FG for readability)
    for disp_row in 0..rows {
        let Some(cells) = display_row(&grid, disp_row, offset) else { continue };
        let y = rect.min.y + disp_row as f32 * char_h;
        for col in 0..cols.min(cells.len()) {
            let cell = &cells[col];
            if cell.ch == ' ' {
                continue;
            }
            let fg = if sel.contains(disp_row, col) {
                DEFAULT_FG
            } else {
                resolve_fg(&cell.fg, cell.bold)
            };
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
    if cursor_blink_on && offset == 0 && grid.cursor_row < rows && grid.cursor_col < cols {
        let cx = rect.min.x + grid.cursor_col as f32 * char_w;
        let cy = rect.min.y + grid.cursor_row as f32 * char_h;
        let cursor_rect = Rect::from_min_size(Pos2::new(cx, cy), Vec2::new(char_w, char_h));

        if has_focus {
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

fn pos_to_cell(
    pos: Pos2,
    rect: Rect,
    char_w: f32,
    char_h: f32,
    rows: usize,
    cols: usize,
) -> (usize, usize) {
    let row_f = (pos.y - rect.min.y) / char_h;
    let col_f = (pos.x - rect.min.x) / char_w;
    let row = if row_f >= 0.0 { row_f.floor() as usize } else { 0 };
    let col = if col_f >= 0.0 { col_f.floor() as usize } else { 0 };
    (row.min(rows.saturating_sub(1)), col.min(cols.saturating_sub(1)))
}

fn extract_selection_text(
    grid: &TerminalGrid,
    sel: &TermSelection,
    rows: usize,
    offset: usize,
) -> String {
    let Some(((sr, sc), (er, ec))) = sel.normalized() else { return String::new() };
    let er = er.min(rows.saturating_sub(1));

    let mut lines: Vec<String> = Vec::new();
    for r in sr..=er {
        let Some(cells) = display_row(grid, r, offset) else { continue };
        let c_start = if r == sr { sc.min(cells.len()) } else { 0 };
        let c_end = if r == er {
            ec.min(cells.len().saturating_sub(1))
        } else {
            cells.len().saturating_sub(1)
        };
        if c_start < cells.len() && c_start <= c_end {
            let line: String = cells[c_start..=c_end].iter().map(|c| c.ch).collect();
            lines.push(line.trim_end().to_string());
        } else {
            lines.push(String::new());
        }
    }
    lines.join("\n")
}

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

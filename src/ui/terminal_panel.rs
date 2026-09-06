use std::time::Duration;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Vec2};
use unicode_width::UnicodeWidthStr;
use crate::core::display_text;
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
const HEADER_LEFT_WIDTH: f32 = 38.0;
const HEADER_RIGHT_WIDTH: f32 = 174.0;

fn terminal_header_rects(header: Rect) -> (Rect, Rect, Rect) {
    let left_edge = (header.left() + HEADER_LEFT_WIDTH).min(header.right());
    let right_edge = (header.right() - HEADER_RIGHT_WIDTH).max(left_edge);
    (
        Rect::from_min_max(header.min, Pos2::new(left_edge, header.bottom())),
        Rect::from_min_max(
            Pos2::new(left_edge, header.top()),
            Pos2::new(right_edge, header.bottom()),
        ),
        Rect::from_min_max(Pos2::new(right_edge, header.top()), header.max),
    )
}

// ── Selection state ───────────────────────────────────────────────────────────

#[derive(Clone, Default)]
struct TermSelection {
    start: Option<(usize, usize)>,   // (display_row, col)
    end:   Option<(usize, usize)>,
}

#[derive(Clone, Default)]
struct TerminalImeState {
    enabled: bool,
    preedit: String,
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
    let active = active.min(terminals.len() - 1);

    let mut event: Option<TerminalPanelEvent> = None;
    let font_id = FontId::new(
        FONT_SIZE,
        crate::platform::fonts::terminal_font_family(),
    );

    // Measure a monospace cell once per frame
    let (char_w, char_h) = ui.fonts(|f| {
        (f.glyph_width(&font_id, 'M'), f.row_height(&font_id))
    });

    // ── Header: tab bar ───────────────────────────────────────────────────────
    let header_height = ui.spacing().interact_size.y;
    let (header_rect, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), header_height),
        Sense::hover(),
    );
    let (new_tab_rect, tabs_rect, actions_rect) = terminal_header_rects(header_rect);

    // Fixed left control: terminal titles can never push New Tab out of reach.
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(new_tab_rect), |ui| {
        ui.set_clip_rect(new_tab_rect);
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            if ui
                .small_button("+")
                .on_hover_text("New terminal tab (opens in current directory)")
                .clicked()
                && event.is_none()
            {
                event = Some(TerminalPanelEvent::NewTab);
            }
        });
    });

    // Variable-width titles scroll only inside the middle viewport.
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tabs_rect), |ui| {
        ui.set_clip_rect(tabs_rect);
        egui::ScrollArea::horizontal()
            .id_salt("terminal_tabs_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (i, term) in terminals.iter().enumerate() {
                        let title = {
                            let g = term.grid.lock().unwrap();
                            if g.title.is_empty() {
                                "zsh".to_string()
                            } else {
                                display_text::normalize(&g.title)
                            }
                        };

                        let resp = ui.selectable_label(i == active, &title);
                        if resp.clicked() && i != active && event.is_none() {
                            event = Some(TerminalPanelEvent::SwitchTab(i));
                        }

                        // Closing and hiding are deliberately different: every
                        // terminal session, including the final one, is closable.
                        let close = ui.small_button("×").on_hover_text("Close terminal tab");
                        if close.clicked() && event.is_none() {
                            event = Some(TerminalPanelEvent::CloseTab(i));
                        }
                        ui.add_space(2.0);
                    }

                    let cwd_str = {
                        let g = terminals[active].grid.lock().unwrap();
                        g.cwd.as_deref().map(display_text::path).unwrap_or_default()
                    };
                    if !cwd_str.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new(&cwd_str).small().weak());
                    }
                });
            });
    });

    // Fixed right controls remain available regardless of title length/count.
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(actions_rect), |ui| {
        ui.set_clip_rect(actions_rect);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("⌘J").small().weak());
            ui.separator();
            if ui
                .small_button("↗ Open in Terminal")
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

    // Custom-painted widgets must explicitly advertise an editable region or
    // winit leaves native IME disabled. Without this, macOS forwards each
    // Korean key as a compatibility Jamo through Event::Text instead of
    // delivering composed text through Event::Ime.
    let ime_id = egui::Id::new("terminal_ime").with(active);
    let mut ime: TerminalImeState = ui.data(|d| d.get_temp(ime_id).unwrap_or_default());
    if has_focus {
        let cursor_rect = {
            let grid = terminals[active].grid.lock().unwrap();
            terminal_cursor_rect(&grid, rect, char_w, char_h)
        };
        let to_global = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        ui.ctx().output_mut(|output| {
            output.ime = Some(egui::output::IMEOutput {
                rect: to_global * rect,
                cursor_rect: to_global * cursor_rect,
            });
        });
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::IMEPurpose(
            egui::viewport::IMEPurpose::Terminal,
        ));
    } else if ime.enabled || !ime.preedit.is_empty() {
        ime = TerminalImeState::default();
        ui.input_mut(|input| {
            input.events.retain(|event| !matches!(event, egui::Event::Ime(_)));
        });
    }

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

        let to_send = ui.input_mut(|input| collect_terminal_input(&mut input.events, &mut ime));

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
    ui.data_mut(|d| d.insert_temp(ime_id, ime.clone()));

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
            let bg = if selection_contains_glyph(&sel, cells, disp_row, col) {
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
            if cell.is_continuation() || (cell.ch == ' ' && cell.combining.is_empty()) {
                continue;
            }
            let fg = if selection_contains_glyph(&sel, cells, disp_row, col) {
                DEFAULT_FG
            } else {
                resolve_fg(&cell.fg, cell.bold)
            };
            let glyph_width = usize::from(cell.width.max(1));
            let glyph_rect = Rect::from_min_size(
                Pos2::new(rect.min.x + col as f32 * char_w, y),
                Vec2::new(glyph_width as f32 * char_w, char_h),
            ).intersect(rect);
            let (glyph_pos, glyph_align) = glyph_paint_anchor(glyph_rect, cell.width);
            painter.with_clip_rect(glyph_rect).text(
                glyph_pos,
                glyph_align,
                cell.glyph_text(),
                font_id.clone(),
                fg,
            );
        }
    }

    // ── Cursor ────────────────────────────────────────────────────────────────
    if cursor_blink_on && offset == 0 && grid.cursor_row < rows && cols > 0 {
        let mut cursor_col = grid.cursor_col.min(cols.saturating_sub(1));
        if grid.cursor_row < grid.cells.len()
            && grid.cells[grid.cursor_row]
                .get(cursor_col)
                .map_or(false, TerminalCell::is_continuation)
        {
            cursor_col = cursor_col.saturating_sub(1);
        }
        let cursor_width = grid.cells
            .get(grid.cursor_row)
            .and_then(|row| row.get(cursor_col))
            .map_or(1, |cell| usize::from(cell.width.max(1)));
        let cx = rect.min.x + cursor_col as f32 * char_w;
        let cy = rect.min.y + grid.cursor_row as f32 * char_h;
        let cursor_rect = Rect::from_min_size(
            Pos2::new(cx, cy),
            Vec2::new(cursor_width as f32 * char_w, char_h),
        ).intersect(rect);

        if has_focus {
            painter.rect_filled(cursor_rect, 0.0, DEFAULT_FG);
            if grid.cursor_row < grid.cells.len() {
                if let Some(cell) = grid.cells[grid.cursor_row].get(cursor_col) {
                    if !cell.is_continuation() && (cell.ch != ' ' || !cell.combining.is_empty()) {
                        let (glyph_pos, glyph_align) = glyph_paint_anchor(cursor_rect, cell.width);
                        painter.with_clip_rect(cursor_rect).text(
                            glyph_pos,
                            glyph_align,
                            cell.glyph_text(),
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

    // Preedit belongs to the native IME, not to the PTY. Draw it at the live
    // cursor until Commit arrives; only the committed UTF-8 text is sent to zsh.
    if has_focus && offset == 0 && !ime.preedit.is_empty() {
        let cursor_rect = terminal_cursor_rect(&grid, rect, char_w, char_h);
        let preedit_cells = UnicodeWidthStr::width(ime.preedit.as_str()).max(1);
        let preedit_rect = Rect::from_min_size(
            cursor_rect.min,
            Vec2::new(preedit_cells as f32 * char_w, char_h),
        ).intersect(rect);
        painter.rect_filled(preedit_rect, 0.0, DEFAULT_BG);
        painter.text(
            preedit_rect.min,
            egui::Align2::LEFT_TOP,
            &ime.preedit,
            font_id.clone(),
            DEFAULT_FG,
        );
        painter.line_segment(
            [preedit_rect.left_bottom(), preedit_rect.right_bottom()],
            egui::Stroke::new(1.0, DEFAULT_FG),
        );
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

fn terminal_cursor_rect(
    grid: &TerminalGrid,
    rect: Rect,
    char_w: f32,
    char_h: f32,
) -> Rect {
    let col = grid.cursor_col.min(grid.cols.saturating_sub(1));
    let row = grid.cursor_row.min(grid.rows.saturating_sub(1));
    Rect::from_min_size(
        Pos2::new(
            rect.min.x + col as f32 * char_w,
            rect.min.y + row as f32 * char_h,
        ),
        Vec2::new(char_w, char_h),
    ).intersect(rect)
}

fn collect_terminal_input(
    events: &mut Vec<egui::Event>,
    ime: &mut TerminalImeState,
) -> Vec<u8> {
    let ime_was_enabled = ime.enabled;
    let frame_has_composition = events.iter().any(|event| {
        matches!(
            event,
            egui::Event::Ime(
                egui::ImeEvent::Enabled
                    | egui::ImeEvent::Preedit(_)
                    | egui::ImeEvent::Commit(_)
            )
        )
    });
    let mut to_send = Vec::new();

    // Process IME events first. macOS can put the final compatibility-Jamo
    // keyboard event and the composed Commit in the same frame.
    for event in events.iter() {
        match event {
            egui::Event::Ime(egui::ImeEvent::Enabled) => {
                ime.enabled = true;
            }
            egui::Event::Ime(egui::ImeEvent::Preedit(text)) => {
                ime.enabled = true;
                ime.preedit.clone_from(text);
            }
            egui::Event::Ime(egui::ImeEvent::Commit(text)) => {
                if text != "\n" && text != "\r" {
                    to_send.extend_from_slice(text.as_bytes());
                }
                ime.enabled = false;
                ime.preedit.clear();
            }
            egui::Event::Ime(egui::ImeEvent::Disabled) => {
                ime.enabled = false;
                ime.preedit.clear();
            }
            _ => {}
        }
    }

    let suppress_text = ime_was_enabled || ime.enabled || frame_has_composition;
    events.retain(|event| {
        match event {
            egui::Event::Ime(_) => false,
            egui::Event::Text(text) => {
                if !suppress_text {
                    to_send.extend_from_slice(text.as_bytes());
                }
                false
            }
            egui::Event::Paste(text) => {
                to_send.extend_from_slice(text.as_bytes());
                false
            }
            egui::Event::Key { key, pressed: true, modifiers, .. } => {
                if ime_was_enabled && is_ime_navigation_key(*key) {
                    return false;
                }
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

    to_send
}

fn is_ime_navigation_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::Backspace
            | egui::Key::ArrowUp
            | egui::Key::ArrowDown
            | egui::Key::ArrowLeft
            | egui::Key::ArrowRight
    )
}

fn glyph_paint_anchor(rect: Rect, width: u8) -> (Pos2, egui::Align2) {
    if width == 2 {
        (Pos2::new(rect.center().x, rect.top()), egui::Align2::CENTER_TOP)
    } else {
        (rect.min, egui::Align2::LEFT_TOP)
    }
}

fn selection_contains_glyph(
    sel: &TermSelection,
    cells: &[TerminalCell],
    row: usize,
    col: usize,
) -> bool {
    if col >= cells.len() {
        return false;
    }
    let lead_col = if cells[col].is_continuation() {
        col.saturating_sub(1)
    } else {
        col
    };
    let width = cells
        .get(lead_col)
        .map_or(1, |cell| usize::from(cell.width.max(1)));
    (lead_col..(lead_col + width).min(cells.len())).any(|glyph_col| sel.contains(row, glyph_col))
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
        let mut c_start = if r == sr { sc.min(cells.len()) } else { 0 };
        if c_start < cells.len() && cells[c_start].is_continuation() {
            c_start = c_start.saturating_sub(1);
        }
        let c_end = if r == er {
            ec.min(cells.len().saturating_sub(1))
        } else {
            cells.len().saturating_sub(1)
        };
        if c_start < cells.len() && c_start <= c_end {
            let line: String = cells[c_start..=c_end]
                .iter()
                .filter(|cell| !cell.is_continuation())
                .fold(String::new(), |mut text, cell| {
                    text.push_str(&cell.glyph_text());
                    text
                });
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

#[cfg(test)]
mod tests {
    use super::{
        collect_terminal_input, extract_selection_text, selection_contains_glyph,
        terminal_header_rects, TermSelection, TerminalImeState, HEADER_LEFT_WIDTH,
        HEADER_RIGHT_WIDTH,
    };
    use crate::core::terminal::{TermPerformer, TerminalGrid};

    fn grid_with_output(text: &str) -> TerminalGrid {
        let mut grid = TerminalGrid::new(12, 2);
        let mut parser = vte::Parser::new();
        let mut performer = TermPerformer { grid: &mut grid };
        for byte in text.as_bytes() {
            parser.advance(&mut performer, *byte);
        }
        grid
    }

    #[test]
    fn selection_highlights_both_halves_and_copies_wide_glyph_once() {
        let grid = grid_with_output("한");
        let selection = TermSelection {
            start: Some((0, 0)),
            end: Some((0, 1)),
        };

        assert!(selection_contains_glyph(
            &selection,
            &grid.cells[0],
            0,
            0,
        ));
        assert!(selection_contains_glyph(
            &selection,
            &grid.cells[0],
            0,
            1,
        ));
        assert_eq!(extract_selection_text(&grid, &selection, 2, 0), "한");
    }

    #[test]
    fn ime_sends_only_committed_hangul_to_the_pty() {
        let mut ime = TerminalImeState::default();
        let mut composing = vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Text("ㅎ".to_string()),
            egui::Event::Ime(egui::ImeEvent::Preedit("하".to_string())),
            egui::Event::Text("ㅏ".to_string()),
        ];

        let bytes = collect_terminal_input(&mut composing, &mut ime);

        assert!(bytes.is_empty());
        assert!(composing.is_empty());
        assert!(ime.enabled);
        assert_eq!(ime.preedit, "하");

        // The raw final key can precede Commit in the same macOS frame. It
        // must not be forwarded in addition to the composed syllable.
        let mut committed = vec![
            egui::Event::Text("ㄴ".to_string()),
            egui::Event::Ime(egui::ImeEvent::Commit("한".to_string())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
        ];

        let bytes = collect_terminal_input(&mut committed, &mut ime);

        assert_eq!(bytes, "한".as_bytes());
        assert!(committed.is_empty());
        assert!(!ime.enabled);
        assert!(ime.preedit.is_empty());
    }

    #[test]
    fn ordinary_text_still_reaches_the_pty_outside_ime() {
        let mut ime = TerminalImeState::default();
        let mut events = vec![egui::Event::Text("cargo run".to_string())];

        let bytes = collect_terminal_input(&mut events, &mut ime);

        assert_eq!(bytes, b"cargo run");
        assert!(events.is_empty());
    }

    #[test]
    fn terminal_titles_cannot_expand_over_fixed_header_controls() {
        let header =
            egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(600.0, 24.0));
        let (new_tab, titles, actions) = terminal_header_rects(header);

        assert_eq!(new_tab.width(), HEADER_LEFT_WIDTH);
        assert_eq!(actions.width(), HEADER_RIGHT_WIDTH);
        assert_eq!(titles.left(), new_tab.right());
        assert_eq!(titles.right(), actions.left());
        assert_eq!(new_tab.union(titles).union(actions), header);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn terminal_korean_fallback_has_wide_glyph_metrics() {
        let context = egui::Context::default();
        crate::platform::fonts::setup_fonts(&context);
        let mut metrics = (0.0, 0.0, 0.0);
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let font = egui::FontId::new(
                    super::FONT_SIZE,
                    crate::platform::fonts::terminal_font_family(),
                );
                metrics = ui.fonts(|fonts| {
                    let cell = fonts.glyph_width(&font, 'M');
                    let korean = fonts
                        .layout_no_wrap("한".to_string(), font.clone(), egui::Color32::WHITE)
                        .size()
                        .x;
                    let word = fonts
                        .layout_no_wrap("한글".to_string(), font, egui::Color32::WHITE)
                        .size()
                        .x;
                    (cell, korean, word)
                });
            });
        });

        assert!(metrics.1 >= metrics.0 * 1.6);
        assert!(metrics.1 <= metrics.0 * 2.0 + 0.5);
        assert!(metrics.2 >= metrics.0 * 3.2);
        assert!(metrics.2 <= metrics.0 * 4.0 + 0.5);
    }
}

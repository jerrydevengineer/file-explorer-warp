use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

use anyhow::Context;
use portable_pty::{CommandBuilder, PtySize};
use unicode_normalization::char::compose;
use unicode_width::UnicodeWidthChar;

// ── Color ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub enum TermColor {
    Default,
    Ansi(u8),      // named colors 0-15
    Indexed(u8),   // 256-color palette
    Rgb(u8, u8, u8),
}

// ── Cell ──────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct TerminalCell {
    pub ch: char,
    /// Extra zero-width scalars that could not be canonically composed with
    /// `ch`, rendered as part of the same terminal glyph.
    pub combining: String,
    /// Display width: 0 = continuation, 1 = normal, 2 = wide leading cell.
    pub width: u8,
    pub fg: TermColor,
    pub bg: TermColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

impl Default for TerminalCell {
    fn default() -> Self {
        Self {
            ch: ' ',
            combining: String::new(),
            width: 1,
            fg: TermColor::Default,
            bg: TermColor::Default,
            bold: false,
            italic: false,
            underline: false,
        }
    }
}

impl TerminalCell {
    pub fn is_continuation(&self) -> bool {
        self.width == 0
    }

    pub fn glyph_text(&self) -> String {
        let mut text = self.ch.to_string();
        text.push_str(&self.combining);
        text
    }
}

// ── Grid ──────────────────────────────────────────────────────────────────────

pub struct TerminalGrid {
    pub cols: usize,
    pub rows: usize,
    pub cells: Vec<Vec<TerminalCell>>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scrollback: VecDeque<Vec<TerminalCell>>,
    pub scroll_offset: usize,
    pub title: String,
    pub cwd: Option<PathBuf>,
    cwd_changed: bool,
    // current SGR state applied to new characters
    cur_fg: TermColor,
    cur_bg: TermColor,
    cur_bold: bool,
    cur_italic: bool,
    cur_underline: bool,
    // scroll region (rows, 0-based, inclusive)
    scroll_top: usize,
    scroll_bottom: usize,
}

impl TerminalGrid {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cells = vec![vec![TerminalCell::default(); cols]; rows];
        Self {
            cols,
            rows,
            cells,
            cursor_row: 0,
            cursor_col: 0,
            scrollback: VecDeque::new(),
            scroll_offset: 0,
            title: String::new(),
            cwd: None,
            cwd_changed: false,
            cur_fg: TermColor::Default,
            cur_bg: TermColor::Default,
            cur_bold: false,
            cur_italic: false,
            cur_underline: false,
            scroll_top: 0,
            scroll_bottom: rows.saturating_sub(1),
        }
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        // Build new grid and copy existing content rather than wiping it.
        // This preserves previous command output when the panel is resized.
        let mut new_cells = vec![vec![TerminalCell::default(); cols]; rows];
        let copy_rows = rows.min(self.cells.len());
        for r in 0..copy_rows {
            let copy_cols = cols.min(self.cells[r].len());
            new_cells[r][..copy_cols].clone_from_slice(&self.cells[r][..copy_cols]);
        }
        self.cols = cols;
        self.rows = rows;
        self.cells = new_cells;
        for row in 0..self.rows {
            self.repair_row(row);
        }
        let blank = self.current_cell();
        for line in &mut self.scrollback {
            line.resize(cols, blank.clone());
            line.truncate(cols);
            Self::repair_cells(line, &blank);
        }
        self.cursor_row = self.cursor_row.min(rows.saturating_sub(1));
        self.cursor_col = self.cursor_col.min(cols.saturating_sub(1));
        self.scroll_top = 0;
        self.scroll_bottom = rows.saturating_sub(1);
    }

    /// Takes the CWD update flag; returns Some(path) if the CWD changed since last call.
    pub fn take_cwd_update(&mut self) -> Option<PathBuf> {
        if self.cwd_changed {
            self.cwd_changed = false;
            self.cwd.clone()
        } else {
            None
        }
    }

    fn current_cell(&self) -> TerminalCell {
        TerminalCell {
            ch: ' ',
            combining: String::new(),
            width: 1,
            fg: self.cur_fg.clone(),
            bg: self.cur_bg.clone(),
            bold: self.cur_bold,
            italic: self.cur_italic,
            underline: self.cur_underline,
        }
    }

    fn continuation_cell(&self) -> TerminalCell {
        let mut cell = self.current_cell();
        cell.width = 0;
        cell
    }

    fn display_width(c: char) -> usize {
        UnicodeWidthChar::width(c).unwrap_or(0).min(2)
    }

    fn previous_lead_position(&self) -> Option<(usize, usize)> {
        if self.cursor_row >= self.rows || self.cols == 0 || self.cursor_col == 0 {
            return None;
        }
        let mut col = self.cursor_col.min(self.cols).saturating_sub(1);
        while col > 0 && self.cells[self.cursor_row][col].is_continuation() {
            col -= 1;
        }
        (!self.cells[self.cursor_row][col].is_continuation()).then_some((self.cursor_row, col))
    }

    /// Compose canonical sequences (including Hangul L/V/T Jamo) into the
    /// previous leading cell. Other zero-width scalars are retained on that
    /// cell so they render and copy without consuming a terminal column.
    fn attach_to_previous(&mut self, c: char) -> bool {
        let Some((row, col)) = self.previous_lead_position() else {
            return false;
        };
        let previous = &mut self.cells[row][col];
        if previous.combining.is_empty() {
            if let Some(composed) = compose(previous.ch, c) {
                previous.ch = composed;
                return true;
            }
        }
        if Self::display_width(c) == 0 {
            previous.combining.push(c);
            return true;
        }
        false
    }

    fn clear_glyph_at(&mut self, row: usize, col: usize) {
        if row >= self.rows || col >= self.cols {
            return;
        }
        let lead_col = if self.cells[row][col].is_continuation() {
            col.saturating_sub(1)
        } else {
            col
        };
        let old_width = self.cells[row][lead_col].width;
        let blank = self.current_cell();
        self.cells[row][lead_col] = blank.clone();
        if old_width == 2 && lead_col + 1 < self.cols {
            self.cells[row][lead_col + 1] = blank;
        }
    }

    fn repair_cells(cells: &mut [TerminalCell], blank: &TerminalCell) {
        let mut col = 0;
        while col < cells.len() {
            match cells[col].width {
                2 if col + 1 < cells.len() => {
                    let mut continuation = cells[col].clone();
                    continuation.ch = ' ';
                    continuation.combining.clear();
                    continuation.width = 0;
                    cells[col + 1] = continuation;
                    col += 2;
                }
                2 => {
                    cells[col] = blank.clone();
                    col += 1;
                }
                0 => {
                    cells[col] = blank.clone();
                    col += 1;
                }
                _ => {
                    cells[col].width = 1;
                    col += 1;
                }
            }
        }
    }

    fn repair_row(&mut self, row: usize) {
        if row >= self.rows {
            return;
        }
        let blank = self.current_cell();
        Self::repair_cells(&mut self.cells[row], &blank);
    }

    fn snap_cursor_from_continuation(&mut self) {
        if self.cursor_row < self.rows
            && self.cursor_col < self.cols
            && self.cells[self.cursor_row][self.cursor_col].is_continuation()
        {
            self.cursor_col = self.cursor_col.saturating_sub(1);
        }
    }

    fn put_char(&mut self, c: char) {
        if self.attach_to_previous(c) {
            return;
        }

        let mut width = Self::display_width(c);
        if width == 0 {
            return;
        }
        if self.cols == 1 {
            width = 1;
        }
        if self.cursor_col >= self.cols || (width == 2 && self.cursor_col + width > self.cols) {
            self.cursor_col = 0;
            self.advance_row();
        }
        if self.cursor_row >= self.rows || self.cursor_col >= self.cols {
            return;
        }

        let row = self.cursor_row;
        let col = self.cursor_col;
        self.clear_glyph_at(row, col);
        if width == 2 {
            self.clear_glyph_at(row, col + 1);
        }

        let mut cell = self.current_cell();
        cell.ch = c;
        cell.width = width as u8;
        self.cells[row][col] = cell;
        if width == 2 {
            self.cells[row][col + 1] = self.continuation_cell();
        }
        self.cursor_col += width;
    }

    fn backspace_cursor(&mut self) {
        if self.cursor_col == 0 {
            return;
        }
        self.cursor_col = self.cursor_col.min(self.cols).saturating_sub(1);
    }

    fn cursor_forward(&mut self, count: usize) {
        self.cursor_col = (self.cursor_col + count).min(self.cols.saturating_sub(1));
    }

    fn cursor_backward(&mut self, count: usize) {
        self.cursor_col = self.cursor_col.saturating_sub(count);
    }

    fn set_cursor_col(&mut self, col: usize) {
        self.cursor_col = col.min(self.cols.saturating_sub(1));
    }

    fn erase_range(&mut self, row: usize, mut start: usize, mut end: usize) {
        if row >= self.rows || start >= end || start >= self.cols {
            return;
        }
        start = start.min(self.cols);
        end = end.min(self.cols);
        if self.cells[row][start].is_continuation() {
            start = start.saturating_sub(1);
        }
        if end < self.cols && self.cells[row][end].is_continuation() {
            end += 1;
        }
        let blank = self.current_cell();
        for col in start..end.min(self.cols) {
            self.cells[row][col] = blank.clone();
        }
        self.repair_row(row);
    }

    fn delete_chars(&mut self, count: usize) {
        if self.cursor_row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        self.snap_cursor_from_continuation();
        let row = self.cursor_row;
        let col = self.cursor_col;
        let mut end = (col + count).min(self.cols);
        if end < self.cols && self.cells[row][end].is_continuation() {
            end += 1;
        }
        let remove_count = end.saturating_sub(col);
        self.cells[row].drain(col..end);
        let blank = self.current_cell();
        self.cells[row].extend(std::iter::repeat(blank).take(remove_count));
        self.repair_row(row);
    }

    fn insert_chars(&mut self, count: usize) {
        if self.cursor_row >= self.rows || self.cursor_col >= self.cols {
            return;
        }
        self.snap_cursor_from_continuation();
        let row = self.cursor_row;
        let col = self.cursor_col;
        let blank = self.current_cell();
        for _ in 0..count.min(self.cols) {
            self.cells[row].insert(col, blank.clone());
        }
        self.cells[row].truncate(self.cols);
        self.repair_row(row);
    }

    fn advance_row(&mut self) {
        if self.cursor_row >= self.scroll_bottom {
            // Push top line of scroll region to scrollback
            let pushed = self.cells[self.scroll_top].clone();
            self.scrollback.push_back(pushed);
            // Scroll lines up within the scroll region
            for r in self.scroll_top..self.scroll_bottom {
                self.cells[r] = self.cells[r + 1].clone();
            }
            self.cells[self.scroll_bottom] = vec![self.current_cell(); self.cols];
        } else {
            self.cursor_row += 1;
        }
    }

    fn erase_in_display(&mut self, mode: u16) {
        let blank = self.current_cell();
        match mode {
            0 => {
                // erase from cursor to end
                self.erase_range(self.cursor_row, self.cursor_col, self.cols);
                for row in (self.cursor_row + 1)..self.rows {
                    self.cells[row] = vec![blank.clone(); self.cols];
                }
            }
            1 => {
                // erase from start to cursor
                for row in 0..self.cursor_row {
                    self.cells[row] = vec![blank.clone(); self.cols];
                }
                let end = self.cursor_col.min(self.cols.saturating_sub(1)) + 1;
                self.erase_range(self.cursor_row, 0, end);
            }
            2 | 3 => {
                // erase entire display
                for row in 0..self.rows {
                    self.cells[row] = vec![blank.clone(); self.cols];
                }
                if mode == 3 {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let blank = self.current_cell();
        match mode {
            0 => {
                self.erase_range(self.cursor_row, self.cursor_col, self.cols);
            }
            1 => {
                let end = self.cursor_col.min(self.cols.saturating_sub(1)) + 1;
                self.erase_range(self.cursor_row, 0, end);
            }
            2 => {
                self.cells[self.cursor_row] = vec![blank; self.cols];
            }
            _ => {}
        }
    }
}

// ── VTE Performer ─────────────────────────────────────────────────────────────

pub struct TermPerformer<'a> {
    pub grid: &'a mut TerminalGrid,
}

impl vte::Perform for TermPerformer<'_> {
    fn print(&mut self, c: char) {
        self.grid.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => {
                self.grid.cursor_col = 0;
            }
            b'\n' | 0x0B | 0x0C => {
                self.grid.advance_row();
            }
            0x08 => {
                // backspace
                self.grid.backspace_cursor();
            }
            0x07 => {} // bell — ignore
            0x09 => {
                // tab — advance to next 8-column boundary
                let next = (self.grid.cursor_col / 8 + 1) * 8;
                self.grid.set_cursor_col(next);
            }
            _ => {}
        }
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        _intermediates: &[u8],
        _ignore: bool,
        action: char,
    ) {
        let g = &mut *self.grid;
        let p: Vec<u16> = params.iter().map(|s| s[0]).collect();
        let p0 = *p.first().unwrap_or(&0);
        let p1 = *p.get(1).unwrap_or(&0);

        match action {
            'A' => {
                // cursor up
                let n = p0.max(1) as usize;
                g.cursor_row = g.cursor_row.saturating_sub(n).max(g.scroll_top);
            }
            'B' | 'e' => {
                // cursor down
                let n = p0.max(1) as usize;
                g.cursor_row = (g.cursor_row + n).min(g.scroll_bottom);
            }
            'C' | 'a' => {
                // cursor forward
                let n = p0.max(1) as usize;
                g.cursor_forward(n);
            }
            'D' => {
                // cursor back
                let n = p0.max(1) as usize;
                g.cursor_backward(n);
            }
            'G' => {
                // cursor horizontal absolute
                g.set_cursor_col((p0.max(1) as usize).saturating_sub(1));
            }
            'd' => {
                // cursor vertical absolute
                g.cursor_row = (p0.max(1) as usize).saturating_sub(1).min(g.rows.saturating_sub(1));
            }
            'H' | 'f' => {
                // cursor position (1-based)
                g.cursor_row = (p0.max(1) as usize).saturating_sub(1).min(g.rows.saturating_sub(1));
                g.set_cursor_col((p1.max(1) as usize).saturating_sub(1));
            }
            'J' => g.erase_in_display(p0),
            'K' => g.erase_in_line(p0),
            'L' => {
                // insert lines
                let n = p0.max(1) as usize;
                let bottom = g.scroll_bottom;
                for _ in 0..n {
                    if bottom < g.rows.saturating_sub(1) {
                        g.cells.remove(bottom);
                    } else {
                        g.cells.pop();
                    }
                    let blank = vec![g.current_cell(); g.cols];
                    g.cells.insert(g.cursor_row, blank);
                }
            }
            'M' => {
                // delete lines
                let n = p0.max(1) as usize;
                for _ in 0..n {
                    g.cells.remove(g.cursor_row);
                    let blank = vec![g.current_cell(); g.cols];
                    g.cells.insert(g.scroll_bottom, blank);
                }
            }
            'P' => {
                // delete characters
                let n = p0.max(1) as usize;
                g.delete_chars(n);
            }
            '@' => {
                // insert characters
                let n = p0.max(1) as usize;
                g.insert_chars(n);
            }
            'r' => {
                // set scrolling region
                let top = (p0.max(1) as usize).saturating_sub(1);
                let bottom = if p1 == 0 {
                    g.rows.saturating_sub(1)
                } else {
                    (p1 as usize).saturating_sub(1).min(g.rows.saturating_sub(1))
                };
                if top < bottom {
                    g.scroll_top = top;
                    g.scroll_bottom = bottom;
                }
                g.cursor_row = 0;
                g.cursor_col = 0;
            }
            'm' => {
                // SGR: select graphic rendition
                if p.is_empty() {
                    Self::sgr_reset(g);
                    return;
                }
                let mut i = 0;
                while i < p.len() {
                    match p[i] {
                        0 => Self::sgr_reset(g),
                        1 => g.cur_bold = true,
                        3 => g.cur_italic = true,
                        4 => g.cur_underline = true,
                        22 => g.cur_bold = false,
                        23 => g.cur_italic = false,
                        24 => g.cur_underline = false,
                        39 => g.cur_fg = TermColor::Default,
                        49 => g.cur_bg = TermColor::Default,
                        n @ 30..=37 => g.cur_fg = TermColor::Ansi((n - 30) as u8),
                        n @ 40..=47 => g.cur_bg = TermColor::Ansi((n - 40) as u8),
                        n @ 90..=97 => g.cur_fg = TermColor::Ansi((n - 90 + 8) as u8),
                        n @ 100..=107 => g.cur_bg = TermColor::Ansi((n - 100 + 8) as u8),
                        38 => {
                            if let Some(&2) = p.get(i + 1) {
                                if p.len() > i + 4 {
                                    g.cur_fg = TermColor::Rgb(p[i+2] as u8, p[i+3] as u8, p[i+4] as u8);
                                    i += 4;
                                }
                            } else if let Some(&5) = p.get(i + 1) {
                                if let Some(&idx) = p.get(i + 2) {
                                    g.cur_fg = TermColor::Indexed(idx as u8);
                                    i += 2;
                                }
                            }
                        }
                        48 => {
                            if let Some(&2) = p.get(i + 1) {
                                if p.len() > i + 4 {
                                    g.cur_bg = TermColor::Rgb(p[i+2] as u8, p[i+3] as u8, p[i+4] as u8);
                                    i += 4;
                                }
                            } else if let Some(&5) = p.get(i + 1) {
                                if let Some(&idx) = p.get(i + 2) {
                                    g.cur_bg = TermColor::Indexed(idx as u8);
                                    i += 2;
                                }
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
            }
            'n' => {
                // DSR — device status report (we don't respond, just ignore)
            }
            'h' | 'l' => {
                // DEC private mode set/reset — ignore for now
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            b"0" | b"2" => {
                // set window title
                if let Some(title_bytes) = params.get(1) {
                    if let Ok(title) = std::str::from_utf8(title_bytes) {
                        self.grid.title = title.to_string();
                    }
                }
            }
            b"7" => {
                // OSC 7: shell reports CWD as "file://hostname/path"
                if let Some(url_bytes) = params.get(1) {
                    if let Ok(url) = std::str::from_utf8(url_bytes) {
                        if let Some(new_path) = parse_osc7_path(url) {
                            if self.grid.cwd.as_deref() != Some(&new_path) {
                                self.grid.cwd = Some(new_path);
                                self.grid.cwd_changed = true;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _params: &vte::Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}

impl TermPerformer<'_> {
    fn sgr_reset(g: &mut TerminalGrid) {
        g.cur_fg = TermColor::Default;
        g.cur_bg = TermColor::Default;
        g.cur_bold = false;
        g.cur_italic = false;
        g.cur_underline = false;
    }
}

// ── OSC 7 URL parser ──────────────────────────────────────────────────────────

fn parse_osc7_path(url: &str) -> Option<PathBuf> {
    // "file://hostname/path/to/dir"  →  "/path/to/dir"
    // "file:///path/to/dir"          →  "/path/to/dir"
    let without_scheme = url.strip_prefix("file://")?;
    // Skip hostname (everything up to the first '/')
    let path_start = without_scheme.find('/')?;
    let encoded = without_scheme[path_start..].as_bytes();
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' && index + 2 < encoded.len() {
            if let (Some(high), Some(low)) = (
                hex_value(encoded[index + 1]),
                hex_value(encoded[index + 2]),
            ) {
                decoded.push((high << 4) | low);
                index += 3;
                continue;
            }
        }
        decoded.push(encoded[index]);
        index += 1;
    }

    #[cfg(unix)]
    {
        Some(PathBuf::from(OsString::from_vec(decoded)))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(decoded).ok().map(PathBuf::from)
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

// ── Terminal State (PTY + background thread) ──────────────────────────────────

pub struct TerminalState {
    pub grid: Arc<Mutex<TerminalGrid>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
}

impl TerminalState {
    pub fn spawn(
        cols: usize,
        rows: usize,
        cwd: &Path,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                cols: cols as u16,
                rows: rows as u16,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("could not allocate pseudo-terminal")?;

        let mut cmd = CommandBuilder::new("zsh");
        cmd.cwd(cwd);
        // Tell zsh it's running inside our terminal
        cmd.env("TERM", "xterm-256color");
        // macOS installs its OSC 7 CWD-reporting hook from /etc/zshrc when
        // TERM_PROGRAM identifies Terminal. We consume only that standard
        // escape sequence; TERM_SESSION_ID remains unset, so Resume/session
        // management is not enabled.
        #[cfg(target_os = "macos")]
        {
            cmd.env("TERM_PROGRAM", "Apple_Terminal");
            cmd.env_remove("TERM_SESSION_ID");
            cmd.env_remove("SHELL_SESSION_ID");
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("could not start zsh in pseudo-terminal")?;

        let grid = Arc::new(Mutex::new(TerminalGrid::new(cols, rows)));

        let grid_clone = grid.clone();
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("could not open pseudo-terminal reader")?;
        std::thread::spawn(move || {
            let mut parser = vte::Parser::new();
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut g = grid_clone.lock().unwrap();
                        let mut performer = TermPerformer { grid: &mut g };
                        for &byte in &buf[..n] {
                            parser.advance(&mut performer, byte);
                        }
                        drop(g);
                        notify();
                    }
                }
            }
        });

        let writer = pair
            .master
            .take_writer()
            .context("could not open pseudo-terminal writer")?;
        Ok(Self {
            grid,
            writer,
            master: pair.master,
            child: Some(child),
        })
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let _ = self.master.resize(PtySize {
            cols: cols as u16,
            rows: rows as u16,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut g) = self.grid.lock() {
            g.resize(cols, rows);
        }
    }

    pub fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
    }

    /// Request termination without waiting on the UI thread. Taking the child
    /// handle makes repeated calls harmless and lets closing a terminal tab
    /// release its PTY resources immediately.
    pub fn terminate(&mut self) {
        terminate_child_nonblocking(&mut self.child);
    }
}

fn terminate_child_nonblocking(
    child_slot: &mut Option<Box<dyn portable_pty::Child + Send + Sync>>,
) {
    let Some(mut child) = child_slot.take() else {
        return;
    };

    if matches!(child.try_wait(), Ok(Some(_))) {
        return;
    }

    let process_id = child.process_id();
    let spawn_result = std::thread::Builder::new()
        .name("terminal-child-terminator".to_string())
        .spawn(move || {
            if let Err(error) = child.kill() {
                eprintln!("[terminal] child termination failed: pid={process_id:?} error={error}");
                return;
            }
            if let Err(error) = child.wait() {
                eprintln!("[terminal] child reap failed: pid={process_id:?} error={error}");
            }
        });

    if let Err(error) = spawn_result {
        eprintln!("[terminal] could not start child terminator: {error}");
    }
}

impl Drop for TerminalState {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_osc7_path, terminate_child_nonblocking, TermPerformer, TerminalGrid};
    use portable_pty::{Child, ChildKiller, ExitStatus};
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    #[derive(Debug)]
    struct MockChild {
        killed: mpsc::Sender<()>,
    }

    #[derive(Debug)]
    struct MockChildKiller {
        killed: mpsc::Sender<()>,
    }

    impl ChildKiller for MockChildKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            let _ = self.killed.send(());
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(Self {
                killed: self.killed.clone(),
            })
        }
    }

    impl ChildKiller for MockChild {
        fn kill(&mut self) -> std::io::Result<()> {
            let _ = self.killed.send(());
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
            Box::new(MockChildKiller {
                killed: self.killed.clone(),
            })
        }
    }

    impl Child for MockChild {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<ExitStatus> {
            Ok(ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            Some(42)
        }
    }

    fn feed(grid: &mut TerminalGrid, text: &str) {
        let mut parser = vte::Parser::new();
        let mut performer = TermPerformer { grid };
        for byte in text.as_bytes() {
            parser.advance(&mut performer, *byte);
        }
    }

    #[test]
    fn child_termination_is_backgrounded_and_idempotent() {
        let (killed_tx, killed_rx) = mpsc::channel();
        let mut child: Option<Box<dyn Child + Send + Sync>> =
            Some(Box::new(MockChild { killed: killed_tx }));

        terminate_child_nonblocking(&mut child);

        assert!(child.is_none());
        killed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("background terminator should kill the child");

        terminate_child_nonblocking(&mut child);
        assert!(killed_rx.recv_timeout(Duration::from_millis(25)).is_err());
    }

    fn assert_no_orphan_continuations(grid: &TerminalGrid, row: usize) {
        for col in 0..grid.cols {
            let cell = &grid.cells[row][col];
            if cell.width == 0 {
                assert!(col > 0, "continuation cannot be the first cell");
                assert_eq!(grid.cells[row][col - 1].width, 2);
            }
            if cell.width == 2 {
                assert!(col + 1 < grid.cols, "wide lead must fit in the row");
                assert_eq!(grid.cells[row][col + 1].width, 0);
            }
        }
    }

    #[test]
    fn osc7_reports_and_drains_a_cwd_change() {
        let mut grid = TerminalGrid::new(80, 24);

        feed(&mut grid, "\x1b]7;file://localhost/tmp\x07");

        assert_eq!(grid.take_cwd_update(), Some(PathBuf::from("/tmp")));
        assert_eq!(grid.take_cwd_update(), None);
    }

    #[test]
    fn osc7_decodes_spaces_apostrophes_and_utf8_paths() {
        assert_eq!(
            parse_osc7_path("file://localhost/tmp/Jerry%27s%20%ED%95%9C%EA%B8%80"),
            Some(PathBuf::from("/tmp/Jerry's 한글")),
        );
    }

    fn row_text(grid: &TerminalGrid, row: usize) -> String {
        grid.cells[row]
            .iter()
            .filter(|cell| !cell.is_continuation())
            .fold(String::new(), |mut text, cell| {
                text.push_str(&cell.glyph_text());
                text
            })
            .trim_end()
            .to_string()
    }

    #[test]
    fn ascii_advances_one_cell() {
        let mut grid = TerminalGrid::new(8, 2);
        grid.put_char('A');

        assert_eq!(grid.cursor_col, 1);
        assert_eq!(grid.cells[0][0].ch, 'A');
        assert_eq!(grid.cells[0][0].width, 1);
    }

    #[test]
    fn precomposed_hangul_uses_two_cells() {
        let mut grid = TerminalGrid::new(8, 2);
        grid.put_char('한');

        assert_eq!(grid.cursor_col, 2);
        assert_eq!(grid.cells[0][0].ch, '한');
        assert_eq!(grid.cells[0][0].width, 2);
        assert_eq!(grid.cells[0][1].width, 0);
    }

    #[test]
    fn nfd_hangul_composes_without_extra_columns() {
        let mut grid = TerminalGrid::new(8, 2);
        for c in "\u{1112}\u{1161}\u{11ab}".chars() {
            grid.put_char(c);
        }

        assert_eq!(grid.cursor_col, 2);
        assert_eq!(grid.cells[0][0].ch, '한');
        assert_eq!(grid.cells[0][0].width, 2);
        assert_eq!(grid.cells[0][1].width, 0);
    }

    #[test]
    fn combining_marks_do_not_consume_a_column() {
        let mut grid = TerminalGrid::new(8, 2);
        grid.put_char('e');
        grid.put_char('\u{301}');
        grid.put_char('x');
        grid.put_char('\u{20dd}');

        assert_eq!(grid.cursor_col, 2);
        assert_eq!(grid.cells[0][0].ch, 'é');
        assert_eq!(grid.cells[0][1].ch, 'x');
        assert_eq!(grid.cells[0][1].combining, "\u{20dd}");
    }

    #[test]
    fn wide_glyph_wraps_before_the_final_column() {
        let mut grid = TerminalGrid::new(4, 2);
        for c in "abc한".chars() {
            grid.put_char(c);
        }

        assert_eq!((grid.cursor_row, grid.cursor_col), (1, 2));
        assert_eq!(grid.cells[0][3].ch, ' ');
        assert_eq!(grid.cells[1][0].ch, '한');
        assert_eq!(grid.cells[1][0].width, 2);
        assert_eq!(grid.cells[1][1].width, 0);
    }

    #[test]
    fn backspace_moves_physical_columns_and_edits_repair_wide_boundaries() {
        let mut grid = TerminalGrid::new(8, 2);
        grid.put_char('한');
        grid.put_char('A');

        grid.backspace_cursor();
        assert_eq!(grid.cursor_col, 2);
        grid.backspace_cursor();
        assert_eq!(grid.cursor_col, 1);

        grid.delete_chars(1);
        assert_eq!(grid.cells[0][0].ch, 'A');
        assert_no_orphan_continuations(&grid, 0);

        let mut insert_grid = TerminalGrid::new(8, 2);
        insert_grid.put_char('한');
        insert_grid.set_cursor_col(1);
        insert_grid.insert_chars(1);
        assert_eq!(insert_grid.cells[0][0].ch, ' ');
        assert_eq!(insert_grid.cells[0][1].ch, '한');
        assert_no_orphan_continuations(&insert_grid, 0);
    }

    #[test]
    fn zsh_style_wide_erase_does_not_enter_the_prompt() {
        let mut grid = TerminalGrid::new(24, 2);
        let prompt_end = "/tmp/project $ ".chars().count();

        // Line editors erase a two-column glyph using physical-column cursor
        // controls. Each Backspace must move exactly one column, even when it
        // temporarily lands on a continuation cell.
        feed(&mut grid, "/tmp/project $ 한\u{8}\u{8}  \u{8}\u{8}");

        assert_eq!(grid.cursor_col, prompt_end);
        assert_eq!(row_text(&grid, 0), "/tmp/project $");
        assert_no_orphan_continuations(&grid, 0);
    }

    #[test]
    fn resize_removes_a_wide_glyph_split_by_the_new_edge() {
        let mut grid = TerminalGrid::new(3, 2);
        grid.put_char('A');
        grid.put_char('한');
        grid.resize(2, 2);

        assert_eq!(grid.cells[0][0].ch, 'A');
        assert_eq!(grid.cells[0][1].ch, ' ');
        assert_no_orphan_continuations(&grid, 0);
    }

    #[test]
    fn erase_expands_across_a_wide_glyph() {
        let mut grid = TerminalGrid::new(6, 2);
        grid.put_char('한');
        grid.put_char('A');

        grid.erase_range(0, 1, 2);

        assert_eq!(grid.cells[0][0].ch, ' ');
        assert_eq!(grid.cells[0][1].ch, ' ');
        assert_eq!(grid.cells[0][2].ch, 'A');
        assert_no_orphan_continuations(&grid, 0);
    }

    #[test]
    fn scrollback_resize_repairs_a_wide_glyph_at_the_edge() {
        let mut grid = TerminalGrid::new(3, 1);
        for c in "A한X".chars() {
            grid.put_char(c);
        }
        assert_eq!(grid.scrollback.len(), 1);

        grid.resize(2, 1);

        assert_eq!(grid.scrollback[0][0].ch, 'A');
        assert_eq!(grid.scrollback[0][1].ch, ' ');
        for col in 0..grid.cols {
            assert_ne!(grid.scrollback[0][col].width, 0);
        }
    }

    #[test]
    fn mixed_ascii_and_hangul_cursor_matches_terminal_columns() {
        let mut grid = TerminalGrid::new(10, 2);
        for c in "A한B".chars() {
            grid.put_char(c);
        }

        assert_eq!(grid.cursor_col, 4);
        assert_eq!(grid.cells[0][0].ch, 'A');
        assert_eq!(grid.cells[0][1].ch, '한');
        assert_eq!(grid.cells[0][2].width, 0);
        assert_eq!(grid.cells[0][3].ch, 'B');
    }
}

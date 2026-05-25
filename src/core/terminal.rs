use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use portable_pty::{CommandBuilder, PtySize};

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
            fg: TermColor::Default,
            bg: TermColor::Default,
            bold: false,
            italic: false,
            underline: false,
        }
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
        self.cols = cols;
        self.rows = rows;
        self.cells = vec![vec![TerminalCell::default(); cols]; rows];
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
            fg: self.cur_fg.clone(),
            bg: self.cur_bg.clone(),
            bold: self.cur_bold,
            italic: self.cur_italic,
            underline: self.cur_underline,
        }
    }

    fn put_char(&mut self, c: char) {
        if self.cursor_col >= self.cols {
            self.cursor_col = 0;
            self.advance_row();
        }
        if self.cursor_row < self.rows {
            let cell = &mut self.cells[self.cursor_row][self.cursor_col];
            cell.ch = c;
            cell.fg = self.cur_fg.clone();
            cell.bg = self.cur_bg.clone();
            cell.bold = self.cur_bold;
            cell.italic = self.cur_italic;
            cell.underline = self.cur_underline;
        }
        self.cursor_col += 1;
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
                for col in self.cursor_col..self.cols {
                    self.cells[self.cursor_row][col] = blank.clone();
                }
                for row in (self.cursor_row + 1)..self.rows {
                    self.cells[row] = vec![blank.clone(); self.cols];
                }
            }
            1 => {
                // erase from start to cursor
                for row in 0..self.cursor_row {
                    self.cells[row] = vec![blank.clone(); self.cols];
                }
                for col in 0..=self.cursor_col.min(self.cols.saturating_sub(1)) {
                    self.cells[self.cursor_row][col] = blank.clone();
                }
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
                for col in self.cursor_col..self.cols {
                    self.cells[self.cursor_row][col] = blank.clone();
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(self.cols.saturating_sub(1)) {
                    self.cells[self.cursor_row][col] = blank.clone();
                }
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
                if self.grid.cursor_col > 0 {
                    self.grid.cursor_col -= 1;
                }
            }
            0x07 => {} // bell — ignore
            0x09 => {
                // tab — advance to next 8-column boundary
                let next = (self.grid.cursor_col / 8 + 1) * 8;
                self.grid.cursor_col = next.min(self.grid.cols.saturating_sub(1));
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
                g.cursor_col = (g.cursor_col + n).min(g.cols.saturating_sub(1));
            }
            'D' => {
                // cursor back
                let n = p0.max(1) as usize;
                g.cursor_col = g.cursor_col.saturating_sub(n);
            }
            'G' => {
                // cursor horizontal absolute
                g.cursor_col = (p0.max(1) as usize).saturating_sub(1).min(g.cols.saturating_sub(1));
            }
            'd' => {
                // cursor vertical absolute
                g.cursor_row = (p0.max(1) as usize).saturating_sub(1).min(g.rows.saturating_sub(1));
            }
            'H' | 'f' => {
                // cursor position (1-based)
                g.cursor_row = (p0.max(1) as usize).saturating_sub(1).min(g.rows.saturating_sub(1));
                g.cursor_col = (p1.max(1) as usize).saturating_sub(1).min(g.cols.saturating_sub(1));
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
                let row = g.cursor_row;
                let col = g.cursor_col;
                for _ in 0..n {
                    if col < g.cells[row].len() {
                        let filler = g.current_cell();
                        g.cells[row].remove(col);
                        g.cells[row].push(filler);
                    }
                }
            }
            '@' => {
                // insert characters
                let n = p0.max(1) as usize;
                let row = g.cursor_row;
                let col = g.cursor_col;
                for _ in 0..n {
                    if g.cells[row].len() >= g.cols {
                        g.cells[row].pop();
                    }
                    let blank = g.current_cell();
                    g.cells[row].insert(col, blank);
                }
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
                        if let Some(path_str) = parse_osc7_path(url) {
                            let new_path = PathBuf::from(path_str);
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

fn parse_osc7_path(url: &str) -> Option<&str> {
    // "file://hostname/path/to/dir"  →  "/path/to/dir"
    // "file:///path/to/dir"          →  "/path/to/dir"
    let without_scheme = url.strip_prefix("file://")?;
    // Skip hostname (everything up to the first '/')
    let path_start = without_scheme.find('/')?;
    Some(&without_scheme[path_start..])
}

// ── Terminal State (PTY + background thread) ──────────────────────────────────

pub struct TerminalState {
    pub grid: Arc<Mutex<TerminalGrid>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

impl TerminalState {
    pub fn spawn(
        cols: usize,
        rows: usize,
        cwd: &Path,
        notify: Arc<dyn Fn() + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system.openpty(PtySize {
            cols: cols as u16,
            rows: rows as u16,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = CommandBuilder::new("zsh");
        cmd.cwd(cwd);
        // Tell zsh it's running inside our terminal
        cmd.env("TERM", "xterm-256color");
        pair.slave.spawn_command(cmd)?;

        let grid = Arc::new(Mutex::new(TerminalGrid::new(cols, rows)));

        let grid_clone = grid.clone();
        let mut reader = pair.master.try_clone_reader()?;
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

        let writer = pair.master.take_writer()?;
        Ok(Self { grid, writer, master: pair.master })
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
}

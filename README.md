# File Explorer for macOS

A native macOS file manager built with Rust and [egui](https://github.com/emilk/egui). It combines a dual-pane file browser, a full Git client, an integrated terminal, and macOS Finder tag support — all in a single lightweight desktop app.

---

## Features

### File Browser
- **Dual-pane split view** — open a second pane side-by-side; drag to resize the divider
- **Multi-tab navigation** — open multiple directories in tabs per pane; drag tabs across panes to reorganize
- **Back/forward history** per tab
- **Sort by** name, size, kind, or date modified (click column headers)
- **Show/hide hidden files** (dotfiles)
- **Quick Look** — press `Space` to preview the selected file

### File Operations
- Create files and folders inline (with name editing)
- Copy, Cut, Paste (`⌘C` / `⌘X` / `⌘V`)
- Move to Trash (`⌘Backspace`)
- Drag-and-drop move between panes
- Reveal in Finder, Open in Terminal, Get Info, Share sheet
- Copy file path to clipboard

### Search
- **Fuzzy search** (`⌘F`) powered by [nucleo](https://github.com/helix-editor/nucleo)
- Searches the current directory tree recursively up to 12 levels deep
- Results update live as you type; press Enter to navigate to the match

### macOS Tags
- Read and write macOS Finder tags (stored as extended attributes)
- Define global named tags with colors in the sidebar
- Filter the file list by tag; Spotlight integration for cross-folder tag search

### Git Panel (`⌘G`)
- Auto-detects the repository root when you navigate into a git project
- **Status** — staged / unstaged / untracked files with diff viewer
- **Branches** — list, checkout, create, delete
- **Stashes** — save, apply, drop
- **Commit log** graph with branch/tag decorations
- **Remote operations** — fetch, pull, push
- **Git config** viewer (global + local)
- Dock the panel at the bottom or as a right sidebar

### Integrated Terminal (`⌘J`)
- Embedded `zsh` shell rendered inside the app window
- Full ANSI/VT100 color support (256-color + true color)
- **Bidirectional CWD sync**: navigate in the browser → `cd` fires in the terminal; `cd` in the terminal → browser follows
- Multiple terminal tabs
- Resizable panel height

### Sidebar
- Bookmarks (drag a folder to add; right-click to remove)
- macOS standard locations (Home, Desktop, Documents, Downloads, Applications)
- Tag palette for filtering

### Preferences (`⌘,`)
- Light / Dark / System theme
- Toggle hidden files
- Choose external terminal app (Auto / Terminal.app / iTerm2 / Warp / Ghostty)

---

## Keyboard Shortcuts

| Action | Shortcut |
|---|---|
| New tab | `⌘T` |
| Close tab | `⌘W` |
| New window | `⌘N` |
| Close right pane | `⌘\` |
| Go back / up | `Backspace` |
| Reload | `⌘R` |
| Toggle hidden files | `⌘⇧.` |
| Switch pane focus | `⌘\`` |
| Switch to tab N | `⌘1`–`⌘9` |
| Search | `⌘F` |
| Toggle Git panel | `⌘G` |
| Toggle terminal | `⌘J` |
| Preferences | `⌘,` |
| Quick Look | `Space` |
| Copy file | `⌘C` |
| Cut file | `⌘X` |
| Paste file | `⌘V` |
| Move to Trash | `⌘Backspace` |

---

## Requirements

- macOS 12.0 or later
- Rust toolchain (edition 2021)
- Xcode Command Line Tools

---

## Building

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Run directly
cargo run
```

To build a proper `.app` bundle with icon:

```bash
cargo install cargo-bundle
cargo bundle --release
```

The output will be at `target/release/bundle/osx/FileExplorer.app`.

---

## Configuration

Settings are saved automatically to `~/.config/file-explorer/config.json`.  
Bookmarks are stored at `~/.config/file-explorer/bookmarks.json`.

---

## Dependencies

| Crate | Purpose |
|---|---|
| `eframe` / `egui` | Immediate-mode GUI framework |
| `git2` | Git operations via libgit2 |
| `portable-pty` | Cross-platform PTY for the embedded terminal |
| `vte` | VT100/ANSI escape sequence parser |
| `nucleo` | Fuzzy file search |
| `walkdir` | Recursive directory walker |
| `xattr` | macOS extended attributes (Finder tags) |
| `plist` | Read/write Apple plist format |
| `arboard` | System clipboard |
| `serde` / `serde_json` | Config serialization |
| `tokio` | Async runtime |
| `chrono` | Date/time formatting |
| `objc2` / `objc2-app-kit` | macOS native APIs (Quick Look, Share sheet, etc.) |
| `anyhow` | Error handling |

---

## License

MIT

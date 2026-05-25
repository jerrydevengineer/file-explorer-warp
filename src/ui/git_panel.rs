use std::path::Path;
use eframe::egui;
use crate::git::{
    repo::{GitStatus, BranchInfo, StashEntry, FileStatus},
    graph::GraphRow,
    diff::FileDiff,
};

const ROW_H: f32 = 22.0;
const LANE_W: f32 = 16.0;

#[derive(Clone, Copy, PartialEq)]
pub enum GitTab {
    Graph,
    Status,
    Commit,
    Branches,
    Stash,
    PushPull,
    Config,
}

pub struct GitPanelState {
    pub tab: GitTab,
    // Graph
    pub graph: Vec<GraphRow>,
    pub graph_selected: Option<usize>,
    // Status
    pub status: Option<GitStatus>,
    pub diff: Option<FileDiff>,
    pub diff_file: Option<(String, bool)>, // (path, staged)
    // Commit
    pub commit_msg: String,
    // Branches
    pub branches: Vec<BranchInfo>,
    pub new_branch_name: String,
    // Stash
    pub stashes: Vec<StashEntry>,
    // Push/Pull
    pub remote_name: String,
    pub op_log: Vec<String>,
    // Config
    pub config_global: Vec<(String, String)>,
    pub config_local: Vec<(String, String)>,
    pub config_loaded: bool,
}

impl Default for GitPanelState {
    fn default() -> Self {
        Self {
            tab: GitTab::Status,
            graph: Vec::new(),
            graph_selected: None,
            status: None,
            diff: None,
            diff_file: None,
            commit_msg: String::new(),
            branches: Vec::new(),
            new_branch_name: String::new(),
            stashes: Vec::new(),
            remote_name: "origin".to_string(),
            op_log: Vec::new(),
            config_global: Vec::new(),
            config_local: Vec::new(),
            config_loaded: false,
        }
    }
}

pub enum GitPanelAction {
    Refresh,
    SwitchTab(GitTab),
    // Status
    StageFile(String),
    UnstageFile(String),
    StageAll,
    UnstageAll,
    SelectDiff(String, bool),
    // Commit
    Commit(String),
    // Branches
    CheckoutBranch(String),
    CreateBranch(String),
    DeleteBranch(String),
    // Stash
    StashSave,
    StashApply(usize),
    StashDrop(usize),
    // Push/Pull
    Fetch,
    Pull,
    Push,
    // Panel position
    TogglePosition,
    // Config
    LoadConfig,
}

static LANE_COLORS: &[egui::Color32] = &[
    egui::Color32::from_rgb(100, 149, 237), // Blue (cornflower)
    egui::Color32::from_rgb(80, 200, 120),  // Green
    egui::Color32::from_rgb(255, 213, 79),  // Yellow
    egui::Color32::from_rgb(240, 90, 90),   // Red
    egui::Color32::from_rgb(180, 100, 220), // Purple
    egui::Color32::from_rgb(255, 165, 0),   // Orange
    egui::Color32::from_rgb(64, 190, 190),  // Teal
];

pub fn show(
    ui: &mut egui::Ui,
    workdir: &Path,
    state: &mut GitPanelState,
    panel_is_right: bool,
) -> Vec<GitPanelAction> {
    let mut actions: Vec<GitPanelAction> = Vec::new();

    // ── Row 1: control buttons (always visible, right-aligned) ───────────────
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("⟳").on_hover_text("Refresh").clicked() {
                actions.push(GitPanelAction::Refresh);
            }
            let pos_tip = if panel_is_right { "Move to bottom" } else { "Move to right side" };
            if ui.small_button("⬚").on_hover_text(pos_tip).clicked() {
                actions.push(GitPanelAction::TogglePosition);
            }
        });
    });

    // ── Row 2: tab buttons (wrap to next line when panel is narrow) ───────────
    let staged_count = state.status.as_ref().map(|s| s.staged.len()).unwrap_or(0);
    ui.horizontal_wrapped(|ui| {
        let tabs: &[(&str, GitTab)] = &[
            ("Graph", GitTab::Graph),
            ("Status", GitTab::Status),
            ("Commit", GitTab::Commit),
            ("Branches", GitTab::Branches),
            ("Stash", GitTab::Stash),
            ("Push/Pull", GitTab::PushPull),
            ("Config", GitTab::Config),
        ];

        for (label, tab) in tabs {
            let display_label = if *tab == GitTab::Status && staged_count > 0 {
                format!("Status ({})", staged_count)
            } else {
                label.to_string()
            };

            let selected = state.tab == *tab;
            if ui.selectable_label(selected, display_label).clicked() {
                if *tab == GitTab::Config && !state.config_loaded {
                    actions.push(GitPanelAction::LoadConfig);
                }
                state.tab = *tab;
                actions.push(GitPanelAction::SwitchTab(*tab));
            }
        }
    });

    ui.separator();

    // ── Route to sub-panels ───────────────────────────────────────────────────
    match state.tab {
        GitTab::Graph => show_graph(ui, state, &mut actions),
        GitTab::Status => show_status(ui, workdir, state, &mut actions),
        GitTab::Commit => show_commit(ui, state, &mut actions),
        GitTab::Branches => show_branches(ui, state, &mut actions),
        GitTab::Stash => show_stash(ui, state, &mut actions),
        GitTab::PushPull => show_push_pull(ui, state, &mut actions),
        GitTab::Config => show_config(ui, state, &mut actions),
    }

    actions
}

// ── Graph tab ─────────────────────────────────────────────────────────────────

fn show_graph(
    ui: &mut egui::Ui,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    if state.graph.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label("No commits to display. Click ⟳ to load.");
        });
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        let mut prev_lower: Vec<(usize, usize)> = Vec::new();

        for (row_idx, row) in state.graph.iter().enumerate() {
            let graph_width = (row.lane_count as f32 * LANE_W + LANE_W).min(128.0);
            let total_width = ui.available_width();

            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(total_width, ROW_H),
                egui::Sense::click(),
            );

            if response.clicked() {
                state.graph_selected = Some(row_idx);
            }

            let is_selected = state.graph_selected == Some(row_idx);

            // Background highlight
            if is_selected {
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(100, 149, 237, 40));
            } else if response.hovered() {
                ui.painter()
                    .rect_filled(rect, 0.0, egui::Color32::from_rgba_unmultiplied(200, 200, 200, 20));
            }

            // Draw graph lanes
            let graph_rect = egui::Rect::from_min_size(rect.min, egui::vec2(graph_width, ROW_H));
            draw_graph_row(ui, graph_rect, row, &prev_lower, is_selected, LANE_COLORS);

            // Draw text portion
            let text_left = rect.left() + graph_width + 4.0;
            let text_rect = egui::Rect::from_min_max(
                egui::pos2(text_left, rect.top()),
                rect.max,
            );

            // Render ref labels (branches/tags)
            let painter = ui.painter();
            let mut label_x = text_left;
            let mid_y = rect.center().y;

            for ref_label in &row.commit.refs {
                use crate::git::graph::RefKind;
                let (bg_color, text_color) = match ref_label.kind {
                    RefKind::Head => (
                        egui::Color32::from_rgb(255, 213, 79),
                        egui::Color32::BLACK,
                    ),
                    RefKind::Local => (
                        egui::Color32::from_rgb(80, 200, 120),
                        egui::Color32::BLACK,
                    ),
                    RefKind::Remote => (
                        egui::Color32::from_rgb(100, 149, 237),
                        egui::Color32::WHITE,
                    ),
                    RefKind::Tag => (
                        egui::Color32::from_rgb(240, 140, 60),
                        egui::Color32::WHITE,
                    ),
                };

                let galley = painter.layout_no_wrap(
                    ref_label.name.clone(),
                    egui::FontId::proportional(10.0),
                    text_color,
                );
                let label_width = galley.size().x + 6.0;
                let label_rect = egui::Rect::from_min_size(
                    egui::pos2(label_x, mid_y - 8.0),
                    egui::vec2(label_width, 16.0),
                );
                painter.rect_filled(label_rect, 3.0, bg_color);
                painter.galley(
                    egui::pos2(label_x + 3.0, mid_y - galley.size().y * 0.5),
                    galley,
                    text_color,
                );
                label_x += label_width + 3.0;
            }

            // Text: oid | message | author | relative time
            let rel_time = relative_time(row.commit.time);
            let text = format!(
                "{} {} · {} · {}",
                row.commit.oid_short,
                row.commit.message,
                row.commit.author,
                rel_time
            );

            let galley = painter.layout_no_wrap(
                text,
                egui::FontId::proportional(12.0),
                ui.visuals().text_color(),
            );
            let text_pos = egui::pos2(label_x + 4.0, mid_y - galley.size().y * 0.5);
            // Clip text to text_rect
            let clip_rect = text_rect;
            let painter_clipped = ui.painter_at(clip_rect);
            painter_clipped.galley(text_pos, galley, ui.visuals().text_color());

            prev_lower = row.lower_edges.clone();
        }
    });

    let _ = actions; // suppress unused warning if no graph actions
}

fn draw_graph_row(
    ui: &mut egui::Ui,
    row_rect: egui::Rect,
    row: &GraphRow,
    prev_lower: &[(usize, usize)],
    _is_selected: bool,
    lane_colors: &[egui::Color32],
) {
    let mid_y = row_rect.center().y;
    let top_y = row_rect.top();
    let bot_y = row_rect.bottom();
    let graph_left = row_rect.left();

    let lane_x = |lane: usize| -> f32 { graph_left + lane as f32 * LANE_W + LANE_W * 0.5 };

    let painter = ui.painter();

    // Upper half: from previous row's lower_edges
    for (from, to) in prev_lower {
        let color = lane_colors[*to % lane_colors.len()];
        painter.line_segment(
            [egui::pos2(lane_x(*from), top_y), egui::pos2(lane_x(*to), mid_y)],
            egui::Stroke::new(1.5, color),
        );
    }

    // Commit circle
    let cx = lane_x(row.lane);
    let color = lane_colors[row.lane % lane_colors.len()];
    painter.circle_filled(egui::pos2(cx, mid_y), 4.0, color);
    painter.circle_stroke(
        egui::pos2(cx, mid_y),
        4.0,
        egui::Stroke::new(1.0, color),
    );

    // Lower half
    for (from, to) in &row.lower_edges {
        let color = lane_colors[*to % lane_colors.len()];
        painter.line_segment(
            [egui::pos2(lane_x(*from), mid_y), egui::pos2(lane_x(*to), bot_y)],
            egui::Stroke::new(1.5, color),
        );
    }
}

fn relative_time(unix_ts: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - unix_ts;
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 86400 * 30 {
        format!("{}d ago", diff / 86400)
    } else if diff < 86400 * 365 {
        format!("{}mo ago", diff / (86400 * 30))
    } else {
        format!("{}y ago", diff / (86400 * 365))
    }
}

// ── Status tab ────────────────────────────────────────────────────────────────

fn show_status(
    ui: &mut egui::Ui,
    _workdir: &Path,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    let available = ui.available_height();
    let list_height = (available * 0.5).min(300.0);

    // Top: file lists
    egui::ScrollArea::vertical()
        .id_salt("status_files")
        .max_height(list_height)
        .show(ui, |ui| {
            if let Some(ref status) = state.status {
                // Staged section
                ui.label(
                    egui::RichText::new(format!("STAGED ({})", status.staged.len()))
                        .small()
                        .color(egui::Color32::from_rgb(80, 200, 120)),
                );

                let staged_files: Vec<(String, String)> = status
                    .staged
                    .iter()
                    .map(|e| (e.path.clone(), status_symbol(&e.status).to_string()))
                    .collect();

                for (path, symbol) in &staged_files {
                    let label = format!("{} {}", symbol, path);
                    let selected = state
                        .diff_file
                        .as_ref()
                        .map(|(p, s)| p == path && *s)
                        .unwrap_or(false);
                    let resp = ui.selectable_label(selected, &label);
                    if resp.clicked() {
                        actions.push(GitPanelAction::SelectDiff(path.clone(), true));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Unstage").clicked() {
                            actions.push(GitPanelAction::UnstageFile(path.clone()));
                            ui.close_menu();
                        }
                    });
                }

                ui.add_space(4.0);

                // Unstaged + untracked section
                let unstaged_count = status.unstaged.len() + status.untracked.len();
                ui.label(
                    egui::RichText::new(format!("UNSTAGED / UNTRACKED ({})", unstaged_count))
                        .small()
                        .color(egui::Color32::from_rgb(240, 90, 90)),
                );

                let mut unstaged_files: Vec<(String, String)> = status
                    .unstaged
                    .iter()
                    .map(|e| (e.path.clone(), status_symbol(&e.status).to_string()))
                    .collect();
                for e in &status.untracked {
                    unstaged_files.push((e.path.clone(), "?".to_string()));
                }

                for (path, symbol) in &unstaged_files {
                    let label = format!("{} {}", symbol, path);
                    let selected = state
                        .diff_file
                        .as_ref()
                        .map(|(p, s)| p == path && !*s)
                        .unwrap_or(false);
                    let resp = ui.selectable_label(selected, &label);
                    if resp.clicked() {
                        actions.push(GitPanelAction::SelectDiff(path.clone(), false));
                    }
                    resp.context_menu(|ui| {
                        if ui.button("Stage").clicked() {
                            actions.push(GitPanelAction::StageFile(path.clone()));
                            ui.close_menu();
                        }
                    });
                }
            } else {
                ui.label("No git status loaded. Click ⟳ to refresh.");
            }
        });

    ui.horizontal(|ui| {
        if ui.button("Stage All").clicked() {
            actions.push(GitPanelAction::StageAll);
        }
        if ui.button("Unstage All").clicked() {
            actions.push(GitPanelAction::UnstageAll);
        }
    });

    ui.separator();

    // Bottom: diff view
    egui::ScrollArea::vertical()
        .id_salt("status_diff")
        .show(ui, |ui| {
            if let Some(ref diff) = state.diff {
                for line in &diff.lines {
                    use crate::git::diff::DiffLineKind;
                    let content = line.content.trim_end_matches('\n');
                    match line.kind {
                        DiffLineKind::Added => {
                            ui.label(
                                egui::RichText::new(content)
                                    .monospace()
                                    .color(egui::Color32::from_rgb(80, 200, 120)),
                            );
                        }
                        DiffLineKind::Removed => {
                            ui.label(
                                egui::RichText::new(content)
                                    .monospace()
                                    .color(egui::Color32::from_rgb(240, 90, 90)),
                            );
                        }
                        DiffLineKind::Header => {
                            ui.label(
                                egui::RichText::new(content)
                                    .monospace()
                                    .color(egui::Color32::from_rgb(100, 149, 237)),
                            );
                        }
                        DiffLineKind::Context => {
                            ui.label(egui::RichText::new(content).monospace());
                        }
                    }
                }
            } else {
                ui.label("Select a file to view diff.");
            }
        });
}

fn status_symbol(status: &FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Modified => "M",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
        FileStatus::Untracked => "?",
        FileStatus::TypeChange => "T",
    }
}

// ── Commit tab ────────────────────────────────────────────────────────────────

fn show_commit(
    ui: &mut egui::Ui,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    let branch_name = state
        .status
        .as_ref()
        .and_then(|s| s.head_branch.as_deref())
        .unwrap_or("(no branch)");

    let is_detached = state
        .status
        .as_ref()
        .map(|s| s.is_detached)
        .unwrap_or(false);

    ui.label(
        egui::RichText::new(format!(
            "Branch: {}{}",
            branch_name,
            if is_detached { " (detached HEAD)" } else { "" }
        ))
        .strong(),
    );

    let staged_count = state
        .status
        .as_ref()
        .map(|s| s.staged.len())
        .unwrap_or(0);

    ui.label(format!("{} file(s) staged", staged_count));

    ui.add_space(4.0);
    ui.label("Commit message:");

    let text_edit = egui::TextEdit::multiline(&mut state.commit_msg)
        .desired_width(f32::INFINITY)
        .desired_rows(4)
        .hint_text("Enter commit message...");
    ui.add(text_edit);

    ui.add_space(4.0);

    let can_commit = !state.commit_msg.trim().is_empty() && staged_count > 0;
    ui.add_enabled_ui(can_commit, |ui| {
        if ui.button("Commit").clicked() {
            let msg = state.commit_msg.trim().to_string();
            actions.push(GitPanelAction::Commit(msg));
            state.commit_msg.clear();
        }
    });

    if !can_commit {
        if staged_count == 0 {
            ui.label(
                egui::RichText::new("Nothing staged to commit.")
                    .small()
                    .color(egui::Color32::GRAY),
            );
        } else {
            ui.label(
                egui::RichText::new("Enter a commit message.")
                    .small()
                    .color(egui::Color32::GRAY),
            );
        }
    }
}

// ── Branches tab ──────────────────────────────────────────────────────────────

fn show_branches(
    ui: &mut egui::Ui,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    // New branch input
    ui.horizontal(|ui| {
        let text_edit = egui::TextEdit::singleline(&mut state.new_branch_name)
            .hint_text("New branch name...")
            .desired_width(180.0);
        ui.add(text_edit);
        let can_create = !state.new_branch_name.trim().is_empty();
        ui.add_enabled_ui(can_create, |ui| {
            if ui.button("New Branch").clicked() {
                let name = state.new_branch_name.trim().to_string();
                actions.push(GitPanelAction::CreateBranch(name));
                state.new_branch_name.clear();
            }
        });
    });

    ui.separator();

    egui::ScrollArea::vertical().show(ui, |ui| {
        // Local branches
        ui.label(
            egui::RichText::new("LOCAL")
                .small()
                .color(egui::Color32::from_rgb(80, 200, 120)),
        );

        let local_branches: Vec<(String, bool)> = state
            .branches
            .iter()
            .filter(|b| !b.is_remote)
            .map(|b| (b.name.clone(), b.is_head))
            .collect();

        for (name, is_head) in &local_branches {
            let label = if *is_head {
                format!("* {}", name)
            } else {
                format!("  {}", name)
            };

            let resp = ui.selectable_label(*is_head, &label);
            resp.context_menu(|ui| {
                if !is_head {
                    if ui.button("Checkout").clicked() {
                        actions.push(GitPanelAction::CheckoutBranch(name.clone()));
                        ui.close_menu();
                    }
                }
                if ui.button("Delete").clicked() {
                    actions.push(GitPanelAction::DeleteBranch(name.clone()));
                    ui.close_menu();
                }
            });
        }

        ui.add_space(4.0);

        // Remote branches
        ui.label(
            egui::RichText::new("REMOTE")
                .small()
                .color(egui::Color32::from_rgb(100, 149, 237)),
        );

        let remote_branches: Vec<String> = state
            .branches
            .iter()
            .filter(|b| b.is_remote)
            .map(|b| b.name.clone())
            .collect();

        for name in &remote_branches {
            ui.label(format!("  {}", name));
        }
    });
}

// ── Stash tab ─────────────────────────────────────────────────────────────────

fn show_stash(
    ui: &mut egui::Ui,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    if ui.button("Stash Current (WIP)").clicked() {
        actions.push(GitPanelAction::StashSave);
    }

    ui.separator();

    if state.stashes.is_empty() {
        ui.label("No stashes.");
        return;
    }

    egui::ScrollArea::vertical().show(ui, |ui| {
        // Collect actions to avoid borrow issues
        let mut apply_indices: Vec<usize> = Vec::new();
        let mut pop_indices: Vec<usize> = Vec::new();
        let mut drop_indices: Vec<usize> = Vec::new();

        for stash in &state.stashes {
            ui.horizontal(|ui| {
                ui.label(format!("[{}] {}", stash.index, stash.message));
                if ui.button("Apply").clicked() {
                    apply_indices.push(stash.index);
                }
                if ui.button("Pop").clicked() {
                    pop_indices.push(stash.index);
                }
                if ui.button("Drop").clicked() {
                    drop_indices.push(stash.index);
                }
            });
        }

        for idx in apply_indices {
            actions.push(GitPanelAction::StashApply(idx));
        }
        for idx in pop_indices {
            // Pop = apply + drop
            actions.push(GitPanelAction::StashApply(idx));
            actions.push(GitPanelAction::StashDrop(idx));
        }
        for idx in drop_indices {
            actions.push(GitPanelAction::StashDrop(idx));
        }
    });
}

// ── Config tab ────────────────────────────────────────────────────────────────

fn show_config(
    ui: &mut egui::Ui,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    if !state.config_loaded {
        ui.centered_and_justified(|ui| {
            ui.spinner();
        });
        return;
    }

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Git configuration").small().weak());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("⟳").on_hover_text("Reload config").clicked() {
                state.config_loaded = false;
                actions.push(GitPanelAction::LoadConfig);
            }
        });
    });
    ui.separator();

    egui::ScrollArea::vertical().id_salt("git_config_scroll").show(ui, |ui| {
        let sections: &[(&str, egui::Color32, &Vec<(String, String)>)] = &[
            ("GLOBAL", egui::Color32::from_rgb(180, 130, 70), &state.config_global),
            ("LOCAL (this repo)", egui::Color32::from_rgb(80, 180, 120), &state.config_local),
        ];

        for (heading, color, entries) in sections {
            ui.label(egui::RichText::new(*heading).small().color(*color));
            ui.add_space(2.0);

            if entries.is_empty() {
                ui.label(egui::RichText::new("  (none)").small().weak().italics());
            } else {
                for (key, value) in entries.iter() {
                    let row_resp = ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(key)
                                .monospace()
                                .small()
                                .strong()
                                .color(ui.visuals().text_color()),
                        );
                        ui.label(egui::RichText::new(" = ").small().weak());
                        ui.label(egui::RichText::new(value).monospace().small());
                    });
                    // Right-click to copy
                    row_resp.response.context_menu(|ui| {
                        if ui.button(format!("Copy key: {}", key)).clicked() {
                            ui.ctx().copy_text(key.clone());
                            ui.close_menu();
                        }
                        if ui.button(format!("Copy value: {}", value)).clicked() {
                            ui.ctx().copy_text(value.clone());
                            ui.close_menu();
                        }
                        if ui.button(format!("Copy pair: {}={}", key, value)).clicked() {
                            ui.ctx().copy_text(format!("{}={}", key, value));
                            ui.close_menu();
                        }
                    });
                }
            }

            ui.add_space(8.0);
        }
    });
}

// ── Push/Pull tab ─────────────────────────────────────────────────────────────

fn show_push_pull(
    ui: &mut egui::Ui,
    state: &mut GitPanelState,
    actions: &mut Vec<GitPanelAction>,
) {
    ui.horizontal(|ui| {
        ui.label("Remote:");
        let text_edit = egui::TextEdit::singleline(&mut state.remote_name)
            .desired_width(100.0);
        ui.add(text_edit);
    });

    ui.add_space(4.0);

    ui.horizontal(|ui| {
        if ui.button("Fetch").clicked() {
            actions.push(GitPanelAction::Fetch);
        }
        if ui.button("Pull").clicked() {
            actions.push(GitPanelAction::Pull);
        }
        if ui.button("Push").clicked() {
            actions.push(GitPanelAction::Push);
        }
    });

    ui.separator();

    ui.label(
        egui::RichText::new("Operation log:")
            .small()
            .color(egui::Color32::GRAY),
    );

    egui::ScrollArea::vertical()
        .id_salt("op_log")
        .show(ui, |ui| {
            for entry in &state.op_log {
                ui.label(egui::RichText::new(entry).monospace().small());
            }
        });
}

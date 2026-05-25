use std::path::Path;
use std::collections::HashMap;

pub struct RefLabel {
    pub name: String,
    pub kind: RefKind,
}

pub enum RefKind {
    Head,
    Local,
    Remote,
    Tag,
}

pub struct CommitInfo {
    pub oid_short: String, // 7 chars
    pub message: String,   // first line only
    pub author: String,
    pub time: i64, // unix timestamp
    pub refs: Vec<RefLabel>,
}

pub struct GraphRow {
    pub commit: CommitInfo,
    pub lane: usize,       // which column this commit sits in
    pub lane_count: usize, // total columns needed
    // lower_edges: (from_col, to_col) segments from mid-row down to bottom.
    // Also used by the NEXT row for its upper-half rendering.
    pub lower_edges: Vec<(usize, usize)>,
}

pub fn build_graph(workdir: &Path, max_commits: usize) -> Vec<GraphRow> {
    let repo = match git2::Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut revwalk = match repo.revwalk() {
        Ok(rw) => rw,
        Err(_) => return Vec::new(),
    };

    let _ = revwalk.set_sorting(git2::Sort::TOPOLOGICAL | git2::Sort::TIME);
    let _ = revwalk.push_glob("refs/heads/*");
    let _ = revwalk.push_head();

    // Build ref_map: oid -> Vec<RefLabel>
    let mut ref_map: HashMap<git2::Oid, Vec<RefLabel>> = HashMap::new();

    // Find HEAD oid
    let head_oid = repo.head().ok().and_then(|h| h.target());

    if let Ok(references) = repo.references() {
        for reference in references.flatten() {
            let full_name = match reference.name() {
                Some(n) => n.to_string(),
                None => continue,
            };

            let target_oid = match reference.target() {
                Some(oid) => oid,
                None => {
                    // Try to peel symbolic refs
                    match reference.symbolic_target() {
                        Some(_) => continue,
                        None => continue,
                    }
                }
            };

            // Determine kind and short name
            let (kind, short_name) = if full_name == "HEAD" {
                continue; // handled via head_oid below
            } else if let Some(rest) = full_name.strip_prefix("refs/heads/") {
                (RefKind::Local, rest.to_string())
            } else if let Some(rest) = full_name.strip_prefix("refs/remotes/") {
                (RefKind::Remote, rest.to_string())
            } else if let Some(rest) = full_name.strip_prefix("refs/tags/") {
                (RefKind::Tag, rest.to_string())
            } else {
                continue;
            };

            ref_map
                .entry(target_oid)
                .or_default()
                .push(RefLabel { name: short_name, kind });
        }
    }

    // Mark HEAD commit
    if let Some(oid) = head_oid {
        ref_map
            .entry(oid)
            .or_default()
            .push(RefLabel { name: "HEAD".to_string(), kind: RefKind::Head });
    }

    // Walk commits
    let mut rows: Vec<GraphRow> = Vec::new();
    // open_lanes: tracks which oid is expected in each lane
    let mut open_lanes: Vec<Option<git2::Oid>> = Vec::new();

    for oid_result in revwalk.take(max_commits) {
        let oid = match oid_result {
            Ok(o) => o,
            Err(_) => continue,
        };

        let commit = match repo.find_commit(oid) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Find commit_lane: position of oid in open_lanes, or first None slot, or extend
        let commit_lane = if let Some(pos) = open_lanes.iter().position(|&x| x == Some(oid)) {
            pos
        } else if let Some(pos) = open_lanes.iter().position(|x| x.is_none()) {
            open_lanes[pos] = Some(oid); // temporarily set for tracking below
            pos
        } else {
            open_lanes.push(Some(oid));
            open_lanes.len() - 1
        };

        // Clear this lane since we're consuming this commit
        open_lanes[commit_lane] = None;

        // Get parents
        let parents: Vec<git2::Oid> = (0..commit.parent_count())
            .filter_map(|i| commit.parent_id(i).ok())
            .collect();

        let mut lower_edges: Vec<(usize, usize)> = Vec::new();

        // Pass-through lanes (lanes that are active but are not the commit lane)
        for (i, slot) in open_lanes.iter().enumerate() {
            if slot.is_some() && i != commit_lane {
                lower_edges.push((i, i));
            }
        }

        // Handle parents
        let mut parents_iter = parents.iter();

        if let Some(&first_parent) = parents_iter.next() {
            // Check if first_parent already in open_lanes
            if let Some(existing_lane) = open_lanes.iter().position(|&x| x == Some(first_parent)) {
                lower_edges.push((commit_lane, existing_lane));
            } else {
                // Reuse commit_lane for first parent
                open_lanes[commit_lane] = Some(first_parent);
                lower_edges.push((commit_lane, commit_lane));
            }
        }

        // Extra parents (merge commits)
        for &extra_parent in parents_iter {
            if let Some(existing_lane) = open_lanes.iter().position(|&x| x == Some(extra_parent)) {
                lower_edges.push((commit_lane, existing_lane));
            } else {
                // Open new lane
                let new_lane = if let Some(pos) = open_lanes.iter().position(|x| x.is_none()) {
                    open_lanes[pos] = Some(extra_parent);
                    pos
                } else {
                    open_lanes.push(Some(extra_parent));
                    open_lanes.len() - 1
                };
                lower_edges.push((commit_lane, new_lane));
            }
        }

        // Compute lane_count
        let max_active = open_lanes
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| if slot.is_some() { Some(i) } else { None })
            .max()
            .map(|i| i + 1)
            .unwrap_or(0);
        let lane_count = (commit_lane + 1).max(max_active);

        // Compact: trim trailing Nones
        while open_lanes.last() == Some(&None) {
            open_lanes.pop();
        }

        // Build CommitInfo
        let oid_short = format!("{:.7}", oid);
        let message = commit
            .message()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .to_string();
        let author = commit.author().name().unwrap_or("?").to_string();
        let time = commit.time().seconds();
        let refs = ref_map.remove(&oid).unwrap_or_default();

        rows.push(GraphRow {
            commit: CommitInfo {
                oid_short,
                message,
                author,
                time,
                refs,
            },
            lane: commit_lane,
            lane_count,
            lower_edges,
        });
    }

    rows
}

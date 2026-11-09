//! Deterministic line diff for reviewable text artefacts.
//!
//! The implementation uses Myers' O(ND) algorithm with a bounded edit distance
//! and a bounded output budget. Large or adversarial inputs fall back to an
//! explicit delete-all/insert-all representation instead of consuming
//! quadratic memory or looping forever. This module never calls a model.

use vp_protocol::types::{TextDiff, TextDiffHunk, TextDiffLine};

/// A larger edit distance would require substantially more trace memory. When
/// exceeded, the caller still gets a correct (truncated) representation.
pub const MAX_EDIT_DISTANCE: usize = 512;
pub const DEFAULT_CONTEXT_LINES: usize = 3;
pub const MAX_CONTEXT_LINES: usize = 20;
pub const DEFAULT_MAX_DIFF_LINES: usize = 4_000;
pub const MAX_DIFF_LINES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawOp {
    Equal,
    Delete,
    Insert,
}

/// Compare two texts and return structured hunks. `max_lines` bounds the number
/// of rendered diff lines, not the counts in the summary.
pub fn diff_text(old: &str, new: &str, context: usize, max_lines: usize) -> TextDiff {
    let old_lines = split_lines(old);
    let new_lines = split_lines(new);
    let context = context.min(MAX_CONTEXT_LINES);
    let max_lines = max_lines.clamp(1, MAX_DIFF_LINES);
    if old_lines == new_lines {
        return TextDiff {
            hunks: Vec::new(),
            additions: 0,
            deletions: 0,
            unchanged: old_lines.len().min(u32::MAX as usize) as u32,
            identical: true,
            truncated: false,
        };
    }

    let (raw_ops, mut truncated) = match myers(&old_lines, &new_lines) {
        Some(ops) => (ops, false),
        None => (replace_all(&old_lines, &new_lines), true),
    };
    let additions = raw_ops
        .iter()
        .filter(|op| matches!(op, RawOp::Insert))
        .count()
        .min(u32::MAX as usize) as u32;
    let deletions = raw_ops
        .iter()
        .filter(|op| matches!(op, RawOp::Delete))
        .count()
        .min(u32::MAX as usize) as u32;
    let unchanged = raw_ops
        .len()
        .saturating_sub(additions as usize + deletions as usize)
        .min(u32::MAX as usize) as u32;

    let change_indices = raw_ops
        .iter()
        .enumerate()
        .filter_map(|(index, op)| (!matches!(op, RawOp::Equal)).then_some(index))
        .collect::<Vec<_>>();
    if change_indices.is_empty() {
        return TextDiff {
            hunks: Vec::new(),
            additions,
            deletions,
            unchanged,
            identical: true,
            truncated,
        };
    }

    let mut ranges = Vec::new();
    let mut start = change_indices[0];
    let mut end = start + 1;
    for index in change_indices.into_iter().skip(1) {
        // Merge changes separated only by a short stretch of equal lines so a
        // hunk does not repeat the same context twice.
        if index <= end + context.saturating_mul(2) {
            end = index + 1;
        } else {
            ranges.push((start, end));
            start = index;
            end = index + 1;
        }
    }
    ranges.push((start, end));

    let mut hunks = Vec::new();
    let mut rendered = 0usize;
    for (start, end) in ranges {
        if rendered >= max_lines {
            truncated = true;
            break;
        }
        let from = start.saturating_sub(context);
        let to = (end + context).min(raw_ops.len());
        let mut lines = Vec::new();
        let (first_old, first_new) = line_numbers_at(&raw_ops, from);
        let mut old_line = first_old;
        let mut new_line = first_new;
        for op in &raw_ops[from..to] {
            match op {
                RawOp::Equal => {
                    lines.push(TextDiffLine {
                        kind: "context".to_string(),
                        old_line: Some(old_line),
                        new_line: Some(new_line),
                        text: old_lines
                            .get(old_line.saturating_sub(1) as usize)
                            .cloned()
                            .unwrap_or_default(),
                    });
                    old_line = old_line.saturating_add(1);
                    new_line = new_line.saturating_add(1);
                }
                RawOp::Delete => {
                    lines.push(TextDiffLine {
                        kind: "delete".to_string(),
                        old_line: Some(old_line),
                        new_line: None,
                        text: old_lines
                            .get(old_line.saturating_sub(1) as usize)
                            .cloned()
                            .unwrap_or_default(),
                    });
                    old_line = old_line.saturating_add(1);
                }
                RawOp::Insert => {
                    lines.push(TextDiffLine {
                        kind: "add".to_string(),
                        old_line: None,
                        new_line: Some(new_line),
                        text: new_lines
                            .get(new_line.saturating_sub(1) as usize)
                            .cloned()
                            .unwrap_or_default(),
                    });
                    new_line = new_line.saturating_add(1);
                }
            }
        }
        if lines.len() > max_lines - rendered {
            lines.truncate(max_lines - rendered);
            truncated = true;
        }
        if lines.is_empty() {
            break;
        }
        let old_lines = lines.iter().filter(|line| line.old_line.is_some()).count();
        let new_lines = lines.iter().filter(|line| line.new_line.is_some()).count();
        let old_start = lines
            .iter()
            .find_map(|line| line.old_line)
            .unwrap_or(first_old);
        let new_start = lines
            .iter()
            .find_map(|line| line.new_line)
            .unwrap_or(first_new);
        rendered += lines.len();
        hunks.push(TextDiffHunk {
            old_start,
            old_lines: old_lines.min(u32::MAX as usize) as u32,
            new_start,
            new_lines: new_lines.min(u32::MAX as usize) as u32,
            lines,
        });
        if rendered >= max_lines {
            truncated = true;
            break;
        }
    }

    TextDiff {
        hunks,
        additions,
        deletions,
        unchanged,
        identical: false,
        truncated,
    }
}

fn split_lines(text: &str) -> Vec<String> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines = text.split('\n').map(ToOwned::to_owned).collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines
}

fn line_numbers_at(ops: &[RawOp], index: usize) -> (u32, u32) {
    let mut old_line = 1u32;
    let mut new_line = 1u32;
    for op in ops.iter().take(index) {
        match op {
            RawOp::Equal => {
                old_line = old_line.saturating_add(1);
                new_line = new_line.saturating_add(1);
            }
            RawOp::Delete => old_line = old_line.saturating_add(1),
            RawOp::Insert => new_line = new_line.saturating_add(1),
        }
    }
    (old_line, new_line)
}

fn replace_all(old: &[String], new: &[String]) -> Vec<RawOp> {
    let mut ops = Vec::with_capacity(old.len() + new.len());
    ops.extend(std::iter::repeat_n(RawOp::Delete, old.len()));
    ops.extend(std::iter::repeat_n(RawOp::Insert, new.len()));
    ops
}

/// Bounded Myers diff. Returns `None` if the edit distance exceeds the cap.
fn myers(old: &[String], new: &[String]) -> Option<Vec<RawOp>> {
    let n = old.len() as isize;
    let m = new.len() as isize;
    if n == 0 {
        return Some(vec![RawOp::Insert; m.max(0) as usize]);
    }
    if m == 0 {
        return Some(vec![RawOp::Delete; n.max(0) as usize]);
    }
    let max_d = (n + m).min(MAX_EDIT_DISTANCE as isize);
    let offset = max_d + 1;
    let size = (2 * max_d + 3) as usize;
    let mut v = vec![0isize; size];
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut final_d: Option<isize> = None;

    for d in 0..=max_d {
        let mut found = false;
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                found = true;
                final_d = Some(d);
                break;
            }
            k += 2;
        }
        trace.push(v.clone());
        if found {
            break;
        }
    }

    let final_d = final_d?;
    let mut x = n;
    let mut y = m;
    let mut ops = Vec::new();
    for d in (0..=final_d).rev() {
        let v = trace.get(d as usize)?;
        let k = x - y;
        let index = (k + offset) as usize;
        let prev_k = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            x -= 1;
            y -= 1;
            ops.push(RawOp::Equal);
        }
        if d > 0 {
            if x == prev_x {
                y -= 1;
                ops.push(RawOp::Insert);
            } else {
                x -= 1;
                ops.push(RawOp::Delete);
            }
        }
    }
    ops.reverse();
    Some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered(diff: &TextDiff) -> Vec<String> {
        diff.hunks
            .iter()
            .flat_map(|hunk| {
                hunk.lines
                    .iter()
                    .map(|line| format!("{} {}", line.kind, line.text))
            })
            .collect()
    }

    #[test]
    fn identical_text_has_no_hunks() {
        let diff = diff_text("alpha\nbeta\n", "alpha\nbeta\n", 3, 100);
        assert!(diff.identical);
        assert!(diff.hunks.is_empty());
        assert_eq!(diff.unchanged, 2);
        assert_eq!(diff.additions, 0);
        assert_eq!(diff.deletions, 0);
    }

    #[test]
    fn modification_is_one_delete_and_one_add() {
        let diff = diff_text("alpha\nbeta\ngamma\n", "alpha\ndelta\ngamma\n", 3, 100);
        assert!(!diff.identical);
        assert_eq!(diff.additions, 1);
        assert_eq!(diff.deletions, 1);
        assert_eq!(diff.hunks.len(), 1);
        let lines = rendered(&diff);
        assert!(lines.contains(&"delete beta".to_string()));
        assert!(lines.contains(&"add delta".to_string()));
        assert!(lines.contains(&"context alpha".to_string()));
        assert!(lines.contains(&"context gamma".to_string()));
        assert_eq!(diff.hunks[0].old_start, 1);
        assert_eq!(diff.hunks[0].old_lines, 3);
        assert_eq!(diff.hunks[0].new_lines, 3);
    }

    #[test]
    fn distant_changes_produce_separate_hunks_and_line_numbers() {
        let old = (1..=20)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut lines = old.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
        lines[1] = "changed two".to_string();
        lines[17] = "changed eighteen".to_string();
        let new = lines.join("\n");
        let diff = diff_text(&old, &new, 1, 100);
        assert_eq!(diff.hunks.len(), 2);
        assert_eq!(diff.additions, 2);
        assert_eq!(diff.deletions, 2);
        assert_eq!(diff.hunks[0].old_start, 1);
        assert_eq!(diff.hunks[0].old_lines, 3);
        assert_eq!(diff.hunks[1].old_start, 17);
        assert_eq!(diff.hunks[1].old_lines, 3);
    }

    #[test]
    fn additions_and_unicode_are_preserved() {
        let diff = diff_text(
            "# 标题\n\n第一段。\n",
            "# 标题\n\n第一段。\n\n第二段 with unicode ✓\n",
            2,
            100,
        );
        assert_eq!(diff.additions, 2);
        assert_eq!(diff.deletions, 0);
        assert!(
            rendered(&diff)
                .iter()
                .any(|line| line.contains("第二段 with unicode ✓"))
        );
    }

    #[test]
    fn output_budget_truncates_without_losing_counts() {
        let old = "a\nb\nc\n";
        let new = "a\nx\ny\nz\nc\n";
        let diff = diff_text(old, new, 1, 2);
        assert!(diff.truncated);
        assert_eq!(diff.additions, 3);
        assert_eq!(diff.deletions, 1);
        assert!(
            diff.hunks
                .iter()
                .map(|hunk| hunk.lines.len())
                .sum::<usize>()
                <= 2
        );
    }

    #[test]
    fn crlf_is_normalized() {
        let diff = diff_text("alpha\r\nbeta\r\n", "alpha\r\ngamma\r\n", 1, 100);
        assert_eq!(diff.additions, 1);
        assert!(rendered(&diff).iter().any(|line| line == "add gamma"));
        assert!(!rendered(&diff).iter().any(|line| line.contains('\r')));
    }

    #[test]
    fn replace_all_fallback_is_bounded() {
        let old = vec!["a".to_string(); 900];
        let new = vec!["b".to_string(); 900];
        let diff = diff_text(&old.join("\n"), &new.join("\n"), 0, 50);
        assert!(diff.truncated);
        assert_eq!(diff.additions, 900);
        assert_eq!(diff.deletions, 900);
        assert!(
            diff.hunks
                .iter()
                .map(|hunk| hunk.lines.len())
                .sum::<usize>()
                <= 50
        );
    }
}

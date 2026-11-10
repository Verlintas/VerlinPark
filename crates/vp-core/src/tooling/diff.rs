//! Compact unified-diff rendering for tool results.

use crate::textdiff::{DEFAULT_MAX_DIFF_LINES, diff_text};

pub(crate) fn render_diff(old: &str, new: &str, context: usize) -> String {
    let diff = diff_text(old, new, context, DEFAULT_MAX_DIFF_LINES);
    if diff.identical {
        return String::new();
    }
    let mut output = String::new();
    if diff.truncated {
        output.push_str("(diff truncated)\n");
    }
    for hunk in &diff.hunks {
        output.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            hunk.old_start, hunk.old_lines, hunk.new_start, hunk.new_lines
        ));
        for line in &hunk.lines {
            let prefix = match line.kind.as_str() {
                "add" => '+',
                "delete" => '-',
                _ => ' ',
            };
            output.push(prefix);
            output.push_str(&line.text);
            output.push('\n');
        }
    }
    if output.len() > 16_000 {
        output.truncate(16_000);
        output.push_str("\n… diff truncated");
    }
    output
}

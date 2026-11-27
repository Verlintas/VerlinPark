//! Bounded shell text helpers shared by permission and contract analysis.
//!
//! The classifiers are syntactic, not a shell. These helpers remove heredoc
//! bodies that are data rather than commands, so test fixtures and generated
//! code are not mistaken for executable lines. Unquoted heredoc delimiters do
//! perform variable/command expansion, so their bodies stay available for the
//! substitution scan; quoted delimiters are skipped entirely.

/// Result of splitting a command into executable text and expansion text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ShellText {
    /// Header lines and ordinary lines. Heredoc bodies are removed.
    pub scan: String,
    /// Same as `scan`, plus unquoted heredoc bodies so command substitutions
    /// inside them are still assessed. Quoted heredoc bodies never appear.
    pub expansion: String,
}

/// Remove heredoc bodies from a command. Used before segment splitting so
/// fixture content is not classified as shell commands.
pub(crate) fn strip_heredoc_bodies(command: &str) -> String {
    split_heredocs(command).scan
}

/// Text that should still be scanned for `$()`/backtick substitutions.
/// Unquoted heredoc bodies are included because the shell expands them.
pub(crate) fn expansion_text(command: &str) -> String {
    split_heredocs(command).expansion
}

fn split_heredocs(command: &str) -> ShellText {
    if !command.contains("<<") {
        return ShellText {
            scan: command.to_string(),
            expansion: command.to_string(),
        };
    }
    let mut scan = String::with_capacity(command.len());
    let mut expansion = String::with_capacity(command.len());
    let mut lines = command.split_inclusive('\n');
    while let Some(line) = lines.next() {
        let delimiters = heredoc_delimiters(line);
        scan.push_str(line);
        expansion.push_str(line);
        for delimiter in delimiters {
            let mut body = String::new();
            let mut terminated = false;
            for candidate in lines.by_ref() {
                let comparable = if delimiter.strip_tabs {
                    candidate.trim_start_matches('\t')
                } else {
                    candidate
                };
                if comparable.trim_end_matches('\n') == delimiter.word {
                    terminated = true;
                    break;
                }
                body.push_str(candidate);
            }
            if !terminated {
                // The shell parses the whole heredoc before running anything;
                // an unterminated heredoc does not execute. Drop the remainder.
                break;
            }
            if !delimiter.quoted {
                expansion.push_str(&body);
            }
        }
    }
    ShellText { scan, expansion }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeredocDelimiter {
    word: String,
    quoted: bool,
    strip_tabs: bool,
}

fn heredoc_delimiters(line: &str) -> Vec<HeredocDelimiter> {
    let chars = line.chars().collect::<Vec<_>>();
    let mut delimiters = Vec::new();
    let mut quote: Option<char> = None;
    let mut index = 0usize;
    while index < chars.len() {
        let ch = chars[index];
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else if ch == '\\' && active == '"' {
                index += 1;
            }
            index += 1;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            index += 1;
            continue;
        }
        if ch == '\\' {
            index += 2;
            continue;
        }
        if ch != '<' || chars.get(index + 1) != Some(&'<') {
            index += 1;
            continue;
        }
        // `<<<` is a here-string, not a heredoc.
        if chars.get(index + 2) == Some(&'<') {
            index += 3;
            continue;
        }
        let mut cursor = index + 2;
        let strip_tabs = chars.get(cursor) == Some(&'-');
        if strip_tabs {
            cursor += 1;
        }
        while chars.get(cursor).is_some_and(|ch| ch.is_whitespace()) {
            cursor += 1;
        }
        let (delimiter, next) = parse_delimiter(&chars, cursor, strip_tabs);
        index = next;
        if let Some(delimiter) = delimiter {
            delimiters.push(delimiter);
        }
    }
    delimiters
}

/// Parse one heredoc delimiter word. Quoted pieces are recognized and removed
/// from the word; a delimiter that contains any quoting is treated as quoted
/// (no expansion), matching shell behavior closely enough for classification.
fn parse_delimiter(
    chars: &[char],
    start: usize,
    strip_tabs: bool,
) -> (Option<HeredocDelimiter>, usize) {
    let mut word = String::new();
    let mut quoted = false;
    let mut index = start;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() || matches!(ch, ';' | '|' | '&' | '<' | '>' | '(' | ')') {
            break;
        }
        match ch {
            '\'' | '"' => {
                quoted = true;
                let quote = ch;
                index += 1;
                while index < chars.len() {
                    let inner = chars[index];
                    if inner == quote {
                        break;
                    }
                    if inner == '\\' && quote == '"' {
                        index += 1;
                        if let Some(escaped) = chars.get(index) {
                            word.push(*escaped);
                        }
                    } else {
                        word.push(inner);
                    }
                    index += 1;
                }
                if index < chars.len() {
                    index += 1;
                }
            }
            '\\' => {
                quoted = true;
                index += 1;
                if let Some(escaped) = chars.get(index) {
                    word.push(*escaped);
                    index += 1;
                }
            }
            _ => {
                word.push(ch);
                index += 1;
            }
        }
    }
    if word.is_empty() {
        return (None, index);
    }
    (
        Some(HeredocDelimiter {
            word,
            quoted,
            strip_tabs,
        }),
        index,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_heredoc_bodies_are_removed_from_scan_and_expansion() {
        let command = "cat > \"$VERLINPARK_SCRATCH/x.py\" <<'EOF'\nrm -rf /etc\n$(whoami)\nEOF\npython3 \"$VERLINPARK_SCRATCH/x.py\"\n";
        let split = split_heredocs(command);
        assert!(!split.scan.contains("rm -rf"));
        assert!(!split.scan.contains("whoami"));
        assert!(!split.expansion.contains("whoami"));
        assert!(split.scan.contains("python3"));
    }

    #[test]
    fn unquoted_heredoc_bodies_remain_in_expansion_text_only() {
        let command = "cat <<EOF\n$(touch /etc/passwd)\nEOF\necho done\n";
        let split = split_heredocs(command);
        assert!(!split.scan.contains("touch"));
        assert!(split.expansion.contains("touch"));
        assert!(split.scan.contains("echo done"));
    }

    #[test]
    fn here_strings_and_missing_delimiters_are_not_heredocs() {
        let command = "cat <<< \"hello\"\n";
        assert_eq!(strip_heredoc_bodies(command), command);
        let empty = "echo <<\n";
        assert_eq!(strip_heredoc_bodies(empty), empty);
    }

    #[test]
    fn dash_delimiters_allow_leading_tabs() {
        let command = "cat <<-EOF\n\tbody\n\tEOF\necho after\n";
        let split = split_heredocs(command);
        assert!(!split.scan.contains("body"));
        assert!(split.scan.contains("echo after"));
    }
}

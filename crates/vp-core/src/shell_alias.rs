//! Bounded expansion of simple scratch-path shell aliases.
//!
//! Agents commonly write `SCRATCH="$VERLINPARK_SCRATCH/check"; rm -rf "$SCRATCH"`.
//! The runtime exports scratch/worktree roots, but a local alias looked like an
//! unanalyzable expansion to the permission and contract classifiers, causing
//! approval and unknown-effect loops for routine verification. This helper
//! resolves only simple assignments whose value is rooted in one of the
//! daemon-provided scratch/worktree paths. Arbitrary variables, command
//! substitution, `$HOME`, and network paths are never expanded.

use std::collections::HashMap;

const EXPANSION_ROUNDS: usize = 8;

/// Resolve simple `NAME="value"`/`NAME=value` assignments whose value is rooted
/// in a daemon-provided scratch/worktree path. Assignments are bounded and only
/// direct `$NAME`/`${NAME}` references are resolved.
pub(crate) fn scratch_aliases(command: &str) -> HashMap<String, String> {
    let mut aliases = HashMap::new();
    let normalized = command.replace("&&", ";").replace("||", ";");
    for raw in normalized.split([';', '\n']) {
        let raw = raw.trim();
        let Some((name, value)) = raw.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !is_shell_variable(name) {
            continue;
        }
        let value = value.trim();
        let value = unquote(value);
        let mut resolved = value.to_string();
        for _ in 0..EXPANSION_ROUNDS {
            let next = expand_variables(&resolved, &aliases);
            if next == resolved {
                break;
            }
            resolved = next;
        }
        if is_scratch_root(&resolved) {
            aliases.insert(name.to_string(), resolved);
        }
    }
    aliases
}

/// Replace direct `$NAME` and `${NAME}` references for already-resolved scratch
/// aliases. Unknown variables and shell metacharacters are left untouched.
pub(crate) fn expand_scratch_aliases(command: &str, aliases: &HashMap<String, String>) -> String {
    if aliases.is_empty() {
        return command.to_string();
    }
    expand_variables(command, aliases)
}

fn expand_variables(input: &str, aliases: &HashMap<String, String>) -> String {
    let bytes = input.as_bytes();
    let mut output = String::with_capacity(input.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' {
            output.push(bytes[index] as char);
            index += 1;
            continue;
        }
        let (name, end) = if bytes.get(index + 1) == Some(&b'{') {
            let start = index + 2;
            let Some(close) = input[start..].find('}') else {
                output.push('$');
                index += 1;
                continue;
            };
            (&input[start..start + close], start + close + 1)
        } else {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            (&input[start..end], end)
        };
        if !name.is_empty()
            && let Some(value) = aliases.get(name)
        {
            output.push_str(value);
            index = end;
            continue;
        }
        output.push('$');
        index += 1;
    }
    output
}

fn unquote(value: &str) -> &str {
    let value = value.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn is_shell_variable(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn is_scratch_root(value: &str) -> bool {
    [
        "$VERLINPARK_SCRATCH",
        "${VERLINPARK_SCRATCH}",
        "$VERLINPARK_WORKTREE",
        "${VERLINPARK_WORKTREE}",
        "$TMPDIR",
        "${TMPDIR}",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_chained_scratch_aliases_only() {
        let command = "SCRATCH=\"$VERLINPARK_SCRATCH/t1-check\"; D=\"$SCRATCH/app\"; rm -rf \"$D\"";
        let aliases = scratch_aliases(command);
        assert_eq!(
            aliases.get("SCRATCH").map(String::as_str),
            Some("$VERLINPARK_SCRATCH/t1-check")
        );
        assert_eq!(
            aliases.get("D").map(String::as_str),
            Some("$VERLINPARK_SCRATCH/t1-check/app")
        );
        let expanded = expand_scratch_aliases(command, &aliases);
        assert!(expanded.contains("rm -rf \"$VERLINPARK_SCRATCH/t1-check/app\""));
        assert!(!aliases.contains_key("HOME"));
    }

    #[test]
    fn leaves_arbitrary_variables_and_metacharacters_alone() {
        let command = "TARGET=/tmp/cache; OTHER=\"$HOME/x\"; echo \"$TARGET $OTHER\"";
        let aliases = scratch_aliases(command);
        assert!(aliases.is_empty());
        assert_eq!(expand_scratch_aliases(command, &aliases), command);
    }
}

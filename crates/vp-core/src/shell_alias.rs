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
    let normalized = normalize_sandbox_parameter_expansions(command)
        .replace("&&", ";")
        .replace("||", ";");
    for raw in normalized.split([';', '\n']) {
        let raw = raw.trim();
        let Some((name, value)) = raw.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if !is_shell_variable(name) {
            continue;
        }
        let value = unquote(value.trim());
        let mut resolved = value.to_string();
        for _ in 0..EXPANSION_ROUNDS {
            let next = expand_variables(&resolved, &aliases);
            if next == resolved {
                break;
            }
            resolved = next;
        }
        if let Some(scratch) = scratch_mktemp_substitution(&resolved) {
            aliases.insert(name.to_string(), scratch.to_string());
            continue;
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
    let normalized = normalize_sandbox_parameter_expansions(command);
    if aliases.is_empty() {
        return normalized;
    }
    expand_variables(&normalized, aliases)
}

/// `NAME="${VERLINPARK_SCRATCH:-$TMPDIR}/x"` is a common safe idiom. Both
/// branches are sandbox roots, but the `:-` form looked like an arbitrary
/// expansion to the contract. Replace the whole parameter expansion with the
/// named sandbox variable when the expression uses a default-value operator.
/// Unrelated parameters are left untouched.
fn normalize_sandbox_parameter_expansions(command: &str) -> String {
    let bytes = command.as_bytes();
    let mut output = String::with_capacity(command.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'$' || bytes.get(index + 1) != Some(&b'{') {
            output.push(bytes[index] as char);
            index += 1;
            continue;
        }
        let start = index + 2;
        let mut depth = 1usize;
        let mut cursor = start;
        while cursor < bytes.len() {
            match bytes[cursor] {
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            cursor += 1;
        }
        if cursor >= bytes.len() {
            output.push('$');
            index += 1;
            continue;
        }
        let body = &command[start..cursor];
        if let Some((name, _)) = body.split_once(":-").or_else(|| body.split_once('-')) {
            let name = name.trim();
            let replacement = match name {
                "VERLINPARK_SCRATCH" | "VERLINPARK_WORKTREE" => Some("$VERLINPARK_SCRATCH"),
                "TMPDIR" => Some("$TMPDIR"),
                _ => None,
            };
            if let Some(replacement) = replacement {
                output.push_str(replacement);
                index = cursor + 1;
                continue;
            }
        }
        output.push_str(&command[index..=cursor]);
        index = cursor + 1;
    }
    output
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

fn scratch_mktemp_substitution(value: &str) -> Option<&'static str> {
    let inner = value.strip_prefix("$(")?.strip_suffix(')')?.trim();
    let mut words = inner.split_whitespace();
    if words.next()? != "mktemp" {
        return None;
    }
    let mut skip_next = false;
    let mut has_template = false;
    for word in words {
        if skip_next {
            skip_next = false;
            continue;
        }
        if matches!(word, "-t" | "--tmpdir") {
            skip_next = true;
            continue;
        }
        if word.starts_with('-') {
            continue;
        }
        has_template = true;
        break;
    }
    if !has_template {
        // Plain `mktemp`/`mktemp -d`/`mktemp -t prefix` creates under TMPDIR,
        // which is one of the daemon-exported sandbox roots. Treating it as
        // arbitrary expansion made routine "copy into a temp dir and check"
        // work look out of scope.
        return Some("$TMPDIR");
    }
    [
        "$VERLINPARK_SCRATCH",
        "${VERLINPARK_SCRATCH}",
        "$VERLINPARK_WORKTREE",
        "${VERLINPARK_WORKTREE}",
        "$TMPDIR",
        "${TMPDIR}",
    ]
    .into_iter()
    .find(|prefix| value.contains(prefix))
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

    #[test]
    fn resolves_mktemp_scratch_command_substitution() {
        let command =
            "D=$(mktemp -d \"$VERLINPARK_SCRATCH/check.XXXXXX\"); cp app/x.py \"$D/x.py\"";
        let aliases = scratch_aliases(command);
        assert_eq!(
            aliases.get("D").map(String::as_str),
            Some("$VERLINPARK_SCRATCH")
        );
        let expanded = expand_scratch_aliases(command, &aliases);
        assert!(expanded.contains("cp app/x.py \"$VERLINPARK_SCRATCH/x.py\""));
    }

    #[test]
    fn chained_mktemp_expands_prior_sandbox_aliases() {
        let command = "S=\"${VERLINPARK_SCRATCH:-$TMPDIR}\"; M=$(mktemp -d \"$S/mut.XXXXXX\") || exit 1; cp -R app tests \"$M\"/";
        let aliases = scratch_aliases(command);
        assert_eq!(
            aliases.get("S").map(String::as_str),
            Some("$VERLINPARK_SCRATCH")
        );
        assert_eq!(
            aliases.get("M").map(String::as_str),
            Some("$VERLINPARK_SCRATCH")
        );
        let expanded = expand_scratch_aliases(command, &aliases);
        assert!(expanded.contains("cp -R app tests \"$VERLINPARK_SCRATCH\"/"));
    }

    #[test]
    fn sandbox_default_parameter_expansions_are_normalized() {
        let command = "S=\"${VERLINPARK_SCRATCH:-$TMPDIR}/x\"; rm -rf \"$S\"";
        let aliases = scratch_aliases(command);
        assert_eq!(
            aliases.get("S").map(String::as_str),
            Some("$VERLINPARK_SCRATCH/x")
        );
        let expanded = expand_scratch_aliases(command, &aliases);
        assert!(expanded.contains("rm -rf \"$VERLINPARK_SCRATCH/x\""));
        let direct = expand_scratch_aliases(
            "rm -rf \"${VERLINPARK_SCRATCH:-$TMPDIR}/y\"",
            &HashMap::new(),
        );
        assert!(direct.contains("$VERLINPARK_SCRATCH/y"));
        let tmp = expand_scratch_aliases("echo \"${TMPDIR:-/tmp}/z\"", &HashMap::new());
        assert!(tmp.contains("$TMPDIR/z"));
        let unrelated = expand_scratch_aliases("echo \"${HOME:-/tmp}\"", &HashMap::new());
        assert_eq!(unrelated, "echo \"${HOME:-/tmp}\"");
    }

    #[test]
    fn plain_mktemp_resolves_to_the_sandbox_tmpdir() {
        for command in [
            "D=$(mktemp -d); cp -R app \"$D\"/",
            "D=$(mktemp -t check); cp -R app \"$D\"/",
            "F=$(mktemp); echo x > \"$F\"",
        ] {
            let aliases = scratch_aliases(command);
            let value = aliases.values().next().map(String::as_str);
            assert_eq!(value, Some("$TMPDIR"), "{command}");
        }
        let explicit = "D=$(mktemp -d /tmp/check.XXXXXX); cp -R app \"$D\"/";
        assert!(scratch_aliases(explicit).is_empty());
    }
}

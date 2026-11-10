//! Deterministic project-memory ranking and brief selection.
//!
//! Memory is a *claim with provenance*, not a chat summary. Ranking is pure
//! Rust over lexical relevance, structural provenance, and observed usage.
//! There are no provider calls, no embeddings, and no hidden model routing.
//!
//! Weights (documented here and covered by tests):
//! - lexical relevance (bm25 or LIKE) ................ 0.60
//! - exact task evidence/tag match ................... 0.60
//! - path evidence/tag prefix overlap ................ 0.15 each, cap 0.45
//! - department tag/evidence match ................... 0.20
//! - kind weight .................................... 0.02–0.30
//! - source weight .................................. 0.00–0.20
//! - recency (30-day half-life) ...................... 0.00–0.25
//! - observed usage (`ln(1+use_count)`) .............. 0.03 each, cap 0.15
//! - pinned ......................................... 0.50
//! - stale multiplier ............................... ×0.25 (only when allowed)
//!
//! Ties are deterministic: score desc, `created_at` desc, `id` asc.

use std::collections::HashSet;

use time::OffsetDateTime;
use vp_protocol::types::{MemoryItem, MemoryMatch};
use vp_store::store::MemoryCandidate;

const LEXICAL_WEIGHT: f64 = 0.60;
const TASK_MATCH_BOOST: f64 = 0.60;
const PATH_MATCH_BOOST: f64 = 0.15;
const PATH_MATCH_CAP: f64 = 0.45;
const DEPARTMENT_MATCH_BOOST: f64 = 0.20;
const RECENCY_WEIGHT: f64 = 0.25;
const USAGE_WEIGHT: f64 = 0.03;
const USAGE_CAP: f64 = 0.15;
const PINNED_BOOST: f64 = 0.50;
const STALE_PENALTY: f64 = 0.25;
const RECENCY_HALF_LIFE_DAYS: f64 = 30.0;
const MAX_BODY_CHARS_PER_ITEM: usize = 420;
const MIN_BODY_CHARS_PER_ITEM: usize = 40;

/// Structural query context. `text` drives lexical retrieval; the other fields
/// boost memories whose provenance matches the current task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryQuery {
    pub text: String,
    pub task_id: Option<String>,
    pub department: Option<String>,
    pub paths: Vec<String>,
}

impl MemoryQuery {
    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
            && self.task_id.is_none()
            && self.department.is_none()
            && self.paths.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BriefSelection {
    pub text: String,
    pub memories: Vec<MemoryMatch>,
}

/// Rank candidates with an injected `now` so tests are stable.
pub fn rank_candidates(
    candidates: Vec<MemoryCandidate>,
    query: &MemoryQuery,
    now: OffsetDateTime,
    include_stale: bool,
) -> Vec<MemoryMatch> {
    let task_id = query
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty());
    let department = query
        .department
        .as_deref()
        .map(str::trim)
        .filter(|department| !department.is_empty());
    let paths = query
        .paths
        .iter()
        .map(|path| normalize_path(path))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();

    let mut matches = Vec::new();
    for candidate in candidates {
        let memory = candidate.memory;
        match memory.status.as_str() {
            "superseded" | "archived" => continue,
            "stale" if !include_stale => continue,
            _ => {}
        }
        let mut reasons = Vec::new();
        let mut seen_reasons = HashSet::new();
        let mut score = candidate.lexical.clamp(0.0, 1.0) * LEXICAL_WEIGHT;
        if candidate.lexical > 0.0 {
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                if candidate.fts { "fts" } else { "like" },
            );
        }

        if let Some(task_id) = task_id
            && memory_matches_task(&memory, task_id)
        {
            score += TASK_MATCH_BOOST;
            push_reason(&mut reasons, &mut seen_reasons, &format!("task:{task_id}"));
        }

        if !paths.is_empty() {
            let mut matched = Vec::new();
            for query_path in &paths {
                if memory_paths(&memory)
                    .iter()
                    .any(|path| path_prefix_overlap(path, query_path))
                {
                    matched.push(query_path.clone());
                }
            }
            if !matched.is_empty() {
                let boost = (PATH_MATCH_BOOST * matched.len() as f64).min(PATH_MATCH_CAP);
                score += boost;
                for path in matched {
                    push_reason(&mut reasons, &mut seen_reasons, &format!("path:{path}"));
                }
            }
        }

        if let Some(department) = department
            && memory_matches_department(&memory, department)
        {
            score += DEPARTMENT_MATCH_BOOST;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("department:{department}"),
            );
        }

        let kind_weight = kind_weight(&memory.kind);
        if kind_weight > 0.0 {
            score += kind_weight;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("kind:{}", memory.kind),
            );
        }
        let source_weight = source_weight(&memory.source);
        if source_weight > 0.0 {
            score += source_weight;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("source:{}", memory.source),
            );
        }

        let recency = recency_factor(&memory.created_at, now);
        if recency > 0.0 {
            score += recency * RECENCY_WEIGHT;
            if recency >= 0.5 {
                push_reason(&mut reasons, &mut seen_reasons, "recent");
            }
        }

        if memory.use_count > 0 {
            let usage = (f64::from(memory.use_count).ln_1p() * USAGE_WEIGHT).min(USAGE_CAP);
            score += usage;
            push_reason(
                &mut reasons,
                &mut seen_reasons,
                &format!("used:{}", memory.use_count),
            );
        }

        if memory.pinned {
            score += PINNED_BOOST;
            push_reason(&mut reasons, &mut seen_reasons, "pinned");
        }

        if memory.status == "stale" {
            score *= STALE_PENALTY;
            push_reason(&mut reasons, &mut seen_reasons, "stale");
        }

        matches.push(MemoryMatch {
            memory,
            score,
            reasons,
        });
    }

    matches.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.memory.created_at.cmp(&left.memory.created_at))
            .then_with(|| left.memory.id.cmp(&right.memory.id))
    });
    matches
}

/// Build a compact brief under a hard character budget. Items are considered in
/// rank order; a low-ranked item is dropped rather than exceeding the budget.
pub fn select_brief(matches: &[MemoryMatch], max_chars: u32) -> BriefSelection {
    let budget = max_chars.max(1) as usize;
    let mut text = String::new();
    let mut selected = Vec::new();
    for matched in matches {
        let prefix = format!(
            "[{}·{}] {}{} · ",
            matched.memory.kind,
            matched.memory.source,
            matched.memory.title,
            provenance_suffix(&matched.memory)
        );
        let prefix_chars = prefix.chars().count();
        let used = text.chars().count();
        if used > 0 && used + 1 >= budget {
            break;
        }
        let separator = usize::from(used > 0);
        let remaining = budget.saturating_sub(used + separator + prefix_chars);
        if remaining < MIN_BODY_CHARS_PER_ITEM {
            break;
        }
        let body_budget = remaining.min(MAX_BODY_CHARS_PER_ITEM);
        let body = collapse_whitespace(&matched.memory.body);
        let body_chars = body.chars().count();
        let body_part = if body_chars > body_budget {
            let mut clipped = body.chars().take(body_budget - 1).collect::<String>();
            clipped.push('…');
            clipped
        } else {
            body
        };
        if used > 0 {
            text.push('\n');
        }
        text.push_str(&prefix);
        text.push_str(&body_part);
        selected.push(matched.clone());
    }
    BriefSelection {
        text,
        memories: selected,
    }
}

/// Human-readable provenance for a memory without exposing raw evidence JSON.
pub fn provenance_label(memory: &MemoryItem) -> String {
    let mut parts = Vec::new();
    if let Some(task) = memory
        .evidence
        .iter()
        .find(|evidence| evidence.kind == "task" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("task {}", task.reference.trim()));
    }
    if let Some(commit) = memory
        .evidence
        .iter()
        .find(|evidence| evidence.kind == "commit" && !evidence.reference.trim().is_empty())
    {
        parts.push(format!("commit {}", short_commit(&commit.reference)));
    }
    parts.join(", ")
}

fn provenance_suffix(memory: &MemoryItem) -> String {
    let label = provenance_label(memory);
    if label.is_empty() {
        String::new()
    } else {
        format!(" ({label})")
    }
}

fn push_reason(reasons: &mut Vec<String>, seen: &mut HashSet<String>, reason: &str) {
    if seen.insert(reason.to_string()) {
        reasons.push(reason.to_string());
    }
}

fn normalize_path(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

fn path_prefix_overlap(left: &str, right: &str) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|rest| rest.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn memory_paths(memory: &MemoryItem) -> Vec<String> {
    let mut paths = memory
        .evidence
        .iter()
        .filter(|evidence| evidence.kind == "path")
        .map(|evidence| normalize_path(&evidence.reference))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    for tag in &memory.tags {
        let tag = tag.strip_prefix("path:").unwrap_or(tag);
        if tag.contains('/') {
            let path = normalize_path(tag);
            if !path.is_empty() {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn memory_matches_task(memory: &MemoryItem, task_id: &str) -> bool {
    memory
        .evidence
        .iter()
        .any(|evidence| evidence.kind == "task" && evidence.reference.trim() == task_id)
        || memory.tags.iter().any(|tag| tag.trim() == task_id)
}

fn memory_matches_department(memory: &MemoryItem, department: &str) -> bool {
    let department = department.to_lowercase();
    memory
        .tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case(&department))
        || memory.evidence.iter().any(|evidence| {
            evidence.kind == "department"
                || evidence.reference.eq_ignore_ascii_case(&department)
                || evidence
                    .note
                    .as_deref()
                    .is_some_and(|note| note.eq_ignore_ascii_case(&department))
        })
}

fn kind_weight(kind: &str) -> f64 {
    match kind {
        "decision" => 0.30,
        "convention" => 0.28,
        "handoff" => 0.26,
        "fact" => 0.20,
        "task_result" => 0.15,
        "scratch" => 0.02,
        "" => 0.0,
        _ => 0.10,
    }
}

fn source_weight(source: &str) -> f64 {
    match source {
        "operator" => 0.20,
        "task_result" => 0.15,
        "orchestrator" => 0.12,
        "agent" => 0.05,
        "import" => 0.0,
        "" => 0.0,
        _ => 0.02,
    }
}

fn recency_factor(created_at: &str, now: OffsetDateTime) -> f64 {
    let Some(created_at) =
        OffsetDateTime::parse(created_at, &time::format_description::well_known::Rfc3339).ok()
    else {
        return 0.0;
    };
    let age_secs = (now - created_at).whole_seconds().max(0) as f64;
    let age_days = age_secs / 86_400.0;
    (0.5_f64)
        .powf(age_days / RECENCY_HALF_LIFE_DAYS)
        .clamp(0.0, 1.0)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn short_commit(reference: &str) -> String {
    reference.trim().chars().take(7).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_protocol::types::MemoryEvidence;

    fn memory(id: &str, kind: &str, title: &str, created_at: &str) -> MemoryItem {
        MemoryItem {
            id: id.to_string(),
            session_id: "ses".to_string(),
            project_id: "prj".to_string(),
            agent_id: "agent".to_string(),
            agent_name: "Agent".to_string(),
            kind: kind.to_string(),
            title: title.to_string(),
            body: "body text".to_string(),
            tags: Vec::new(),
            created_at: created_at.to_string(),
            status: "active".to_string(),
            source: "agent".to_string(),
            evidence: Vec::new(),
            supersedes: Vec::new(),
            superseded_by: None,
            pinned: false,
            use_count: 0,
            last_used_at: None,
            last_verified_at: None,
            verify_after: None,
            stale_reason: None,
        }
    }

    fn candidate(memory: MemoryItem, lexical: f64) -> MemoryCandidate {
        MemoryCandidate {
            memory,
            lexical,
            fts: true,
        }
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse(
            "2026-11-11T00:00:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .unwrap()
    }

    #[test]
    fn ranks_deterministically_with_task_and_path_boosts() {
        let mut task_memory = memory("m-task", "handoff", "Task handoff", "2026-11-01T00:00:00Z");
        task_memory.evidence = vec![
            MemoryEvidence {
                kind: "task".to_string(),
                reference: "T3".to_string(),
                note: None,
            },
            MemoryEvidence {
                kind: "path".to_string(),
                reference: "src/api".to_string(),
                note: None,
            },
        ];
        let plain = memory("m-plain", "fact", "Plain fact", "2026-11-10T00:00:00Z");
        let query = MemoryQuery {
            text: "handoff".to_string(),
            task_id: Some("T3".to_string()),
            department: None,
            paths: vec!["src/api/handlers".to_string()],
        };
        let ranked = rank_candidates(
            vec![
                candidate(plain.clone(), 0.4),
                candidate(task_memory.clone(), 0.3),
            ],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "m-task");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "task:T3"));
        assert!(
            ranked[0]
                .reasons
                .iter()
                .any(|reason| reason == "path:src/api/handlers")
        );

        let again = rank_candidates(
            vec![candidate(plain, 0.4), candidate(task_memory, 0.3)],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked, again);
    }

    #[test]
    fn pinned_and_stale_penalty_are_visible_in_reasons() {
        let mut pinned = memory("m-pinned", "fact", "Pinned", "2026-10-01T00:00:00Z");
        pinned.pinned = true;
        let mut stale = memory("m-stale", "fact", "Stale", "2026-11-10T00:00:00Z");
        stale.status = "stale".to_string();
        let query = MemoryQuery {
            text: "pinned".to_string(),
            ..MemoryQuery::default()
        };
        let ranked = rank_candidates(
            vec![
                candidate(stale.clone(), 0.5),
                candidate(pinned.clone(), 0.1),
            ],
            &query,
            now(),
            true,
        );
        assert_eq!(ranked[0].memory.id, "m-pinned");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "pinned"));
        let stale_match = ranked
            .iter()
            .find(|matched| matched.memory.id == "m-stale")
            .unwrap();
        assert!(stale_match.reasons.iter().any(|reason| reason == "stale"));
        let excluded = rank_candidates(vec![candidate(stale, 0.5)], &query, now(), false);
        assert!(excluded.is_empty());
    }

    #[test]
    fn brief_respects_char_budget_and_formats_provenance() {
        let mut task = memory(
            "m1",
            "decision",
            "Keep merges deterministic",
            "2026-11-10T00:00:00Z",
        );
        task.body = "The Integrator must not depend on an LLM. ".repeat(80);
        task.evidence = vec![
            MemoryEvidence {
                kind: "task".to_string(),
                reference: "T3".to_string(),
                note: None,
            },
            MemoryEvidence {
                kind: "commit".to_string(),
                reference: "abcdef1234567890".to_string(),
                note: None,
            },
        ];
        let matches = vec![MemoryMatch {
            memory: task,
            score: 1.0,
            reasons: vec!["fts".to_string()],
        }];
        let selection = select_brief(&matches, 300);
        assert!(selection.text.chars().count() <= 300);
        assert!(selection.text.starts_with("[decision·agent]"));
        assert!(selection.text.contains("(task T3, commit abcdef1)"));
        assert!(selection.text.ends_with('…'));
        assert_eq!(selection.memories.len(), 1);

        let tiny = select_brief(&matches, 10);
        assert!(tiny.text.is_empty());
        assert!(tiny.memories.is_empty());
    }

    #[test]
    fn recency_half_life_and_usage_are_monotonic() {
        let fresh = memory("fresh", "fact", "Fresh", "2026-11-10T00:00:00Z");
        let old = memory("old", "fact", "Old", "2026-08-01T00:00:00Z");
        let query = MemoryQuery::default();
        let ranked = rank_candidates(
            vec![candidate(old, 0.0), candidate(fresh.clone(), 0.0)],
            &query,
            now(),
            false,
        );
        assert_eq!(ranked[0].memory.id, "fresh");
        assert!(ranked[0].reasons.iter().any(|reason| reason == "recent"));

        let mut used = fresh;
        used.id = "used".to_string();
        used.use_count = 9;
        let ranked = rank_candidates(vec![candidate(used, 0.0)], &query, now(), false);
        assert!(ranked[0].reasons.iter().any(|reason| reason == "used:9"));
        assert!(ranked[0].score > 0.0);
    }
}

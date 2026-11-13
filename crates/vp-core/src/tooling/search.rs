//! Deterministic tool-search retrieval for C8.
//!
//! This module is pure: it never reads a project, calls a provider, or changes
//! a tool surface. `run_tool_search` supplies the already role-filtered tool
//! catalog and uses the result to build a bounded, explainable set.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

pub const BM25_K1: f64 = 1.2;
pub const BM25_B: f64 = 0.75;
pub const MAX_PREREQUISITE_ADDITIONS: usize = 64;
pub const MAX_MISSING_PREREQUISITES: usize = 20;

/// One searchable tool. `key` is an internal stable id (`builtin:name` or
/// `mcp:server:tool`) and `name` is the callable display name.
#[derive(Debug, Clone, Copy)]
pub struct SearchDocument<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub task_kind: &'a str,
    pub category: &'a str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoredDocument {
    pub key: String,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrerequisiteAddition {
    pub name: String,
    pub required_by: Vec<String>,
    /// One for a direct prerequisite, two for a prerequisite of a prerequisite.
    pub depth: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingPrerequisite {
    pub name: String,
    pub reason: String,
    pub required_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrerequisiteClosure {
    pub additions: Vec<PrerequisiteAddition>,
    pub missing: Vec<MissingPrerequisite>,
    pub truncated: bool,
}

/// Discovery prerequisites, not authorization. The runtime's role policy,
/// contracts, approvals, sandbox, and write scope stay authoritative. These
/// edges make a returned call set explainable and complete enough to execute:
/// e.g. editing a file usually starts with reading it.
pub fn prerequisites(name: &str) -> &'static [&'static str] {
    match name {
        "edit_file" | "write_file" | "apply_patch" => &["read_file"],
        "file_manage" => &["list_dir"],
        "run_project_task" => &["project_profile"],
        "build_context" => &["repo_map"],
        "affected_tests" => &["dependency_graph"],
        "find_references" | "find_symbol" => &["repo_map"],
        "search_sources" | "read_source" => &["list_sources"],
        "delegate" | "delegate_many" | "delegate_acp" => &["team_status"],
        _ => &[],
    }
}

fn stem(mut token: String) -> String {
    if token.len() > 3
        && token.ends_with('s')
        && !token.ends_with("ss")
        && !token.ends_with("us")
        && !token.ends_with("is")
    {
        token.pop();
    }
    token
}

fn tokenize(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| stem(token.to_ascii_lowercase()))
        .filter(|token| !token.is_empty())
        .collect()
}

fn document_tokens(document: &SearchDocument<'_>) -> Vec<String> {
    let mut tokens = Vec::new();
    // Field repetition is a deterministic lightweight field boost. BM25 is
    // still computed over the combined document; no field value is exposed.
    for _ in 0..3 {
        tokens.extend(tokenize(document.name));
    }
    for _ in 0..2 {
        tokens.extend(tokenize(document.category));
        tokens.extend(tokenize(document.task_kind));
    }
    tokens.extend(tokenize(document.description));
    tokens
}

/// BM25 ranking over a caller-supplied role-filtered catalog. Query tokens
/// are stemmed with the same bounded rule as document tokens. An empty query
/// returns the catalog deterministically so callers can still enumerate it.
pub fn rank_documents(
    documents: &[SearchDocument<'_>],
    query: &str,
    task_kind: Option<&str>,
    limit: usize,
) -> Vec<ScoredDocument> {
    if documents.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut query_text = query.trim().to_ascii_lowercase();
    if let Some(task_kind) = task_kind {
        let task_kind = task_kind.trim();
        if !task_kind.is_empty() {
            query_text.push(' ');
            query_text.push_str(task_kind);
        }
    }
    let query_tokens = tokenize(&query_text).into_iter().collect::<BTreeSet<_>>();
    if query_tokens.is_empty() {
        let mut all = documents
            .iter()
            .map(|document| ScoredDocument {
                key: document.key.to_string(),
                score: 1.0,
            })
            .collect::<Vec<_>>();
        all.sort_by(|left, right| left.key.cmp(&right.key));
        all.truncate(limit.min(100));
        return all;
    }
    let tokenized = documents.iter().map(document_tokens).collect::<Vec<_>>();
    let count = documents.len() as f64;
    let average_length =
        tokenized.iter().map(|tokens| tokens.len()).sum::<usize>() as f64 / count.max(1.0);
    let mut document_frequency = BTreeMap::<String, usize>::new();
    for tokens in &tokenized {
        let unique = tokens.iter().cloned().collect::<BTreeSet<_>>();
        for token in unique {
            if query_tokens.contains(&token) {
                *document_frequency.entry(token).or_default() += 1;
            }
        }
    }
    let idf = |token: &str| {
        let frequency = document_frequency.get(token).copied().unwrap_or(0) as f64;
        (1.0 + (count - frequency + 0.5) / (frequency + 0.5)).ln()
    };
    let normalized_query = query_text.trim().to_ascii_lowercase();
    let mut scored = Vec::new();
    for (document, tokens) in documents.iter().zip(tokenized.iter()) {
        if tokens.is_empty() {
            continue;
        }
        let mut score = 0.0;
        for token in &query_tokens {
            let term_frequency = tokens
                .iter()
                .filter(|candidate| *candidate == token)
                .count() as f64;
            if term_frequency == 0.0 {
                continue;
            }
            let length = tokens.len() as f64;
            let numerator = term_frequency * (BM25_K1 + 1.0);
            let denominator = term_frequency
                + BM25_K1 * (1.0 - BM25_B + BM25_B * length / average_length.max(1.0));
            score += idf(token) * numerator / denominator.max(f64::EPSILON);
        }
        if score <= 0.0 {
            continue;
        }
        let name = document.name.to_ascii_lowercase();
        if name == normalized_query {
            score += 5.0;
        } else if name.contains(&normalized_query) {
            score += 2.0;
        }
        scored.push(ScoredDocument {
            key: document.key.to_string(),
            score: score.max(0.0),
        });
    }
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.key.cmp(&right.key))
    });
    scored.truncate(limit.min(100));
    scored
}

/// Add prerequisite closure over the role-filtered catalog. Additions are
/// deterministic and bounded; missing prerequisites are explicit evidence so
/// a caller never has to guess whether a set was complete.
pub fn prerequisite_closure(
    selected_names: &[String],
    available_names: &BTreeSet<String>,
    max_additions: usize,
) -> PrerequisiteClosure {
    let mut selected = selected_names.iter().cloned().collect::<BTreeSet<String>>();
    let mut additions = BTreeMap::<String, PrerequisiteAddition>::new();
    let mut missing = BTreeMap::<String, MissingPrerequisite>::new();
    let mut queue = VecDeque::new();
    for name in selected_names {
        queue.push_back((name.clone(), 0u8));
    }
    while let Some((parent, depth)) = queue.pop_front() {
        if depth >= 3 {
            continue;
        }
        for prerequisite in prerequisites(&parent) {
            let name = (*prerequisite).to_string();
            if selected.contains(&name) {
                continue;
            }
            if !available_names.contains(&name) {
                let entry = missing
                    .entry(name.clone())
                    .or_insert_with(|| MissingPrerequisite {
                        name,
                        reason: "not_available_for_role".to_string(),
                        required_by: Vec::new(),
                    });
                if !entry.required_by.contains(&parent) {
                    entry.required_by.push(parent.clone());
                }
                continue;
            }
            if additions.len() >= max_additions.min(MAX_PREREQUISITE_ADDITIONS) {
                let entry = missing
                    .entry("prerequisite_closure".to_string())
                    .or_insert_with(|| MissingPrerequisite {
                        name: "prerequisite_closure".to_string(),
                        reason: "result_cap".to_string(),
                        required_by: Vec::new(),
                    });
                if !entry.required_by.contains(&parent) {
                    entry.required_by.push(parent.clone());
                }
                continue;
            }
            selected.insert(name.clone());
            let entry = additions
                .entry(name.clone())
                .or_insert_with(|| PrerequisiteAddition {
                    name: name.clone(),
                    required_by: Vec::new(),
                    depth,
                });
            entry.depth = entry.depth.min(depth.saturating_add(1));
            if !entry.required_by.contains(&parent) {
                entry.required_by.push(parent.clone());
            }
            queue.push_back((name, depth.saturating_add(1)));
        }
    }
    let mut additions = additions.into_values().collect::<Vec<_>>();
    additions.sort_by(|left, right| left.name.cmp(&right.name));
    let mut missing = missing.into_values().collect::<Vec<_>>();
    missing.sort_by(|left, right| left.name.cmp(&right.name));
    let truncated = missing.len() > MAX_MISSING_PREREQUISITES;
    missing.truncate(MAX_MISSING_PREREQUISITES);
    for entry in &mut additions {
        entry.required_by.sort();
        entry.required_by.dedup();
        entry.required_by.truncate(8);
    }
    for entry in &mut missing {
        entry.required_by.sort();
        entry.required_by.dedup();
        entry.required_by.truncate(8);
    }
    PrerequisiteClosure {
        additions,
        missing,
        truncated,
    }
}

/// Deterministic completeness estimate over the returned set and explicit
/// missing prerequisites. This is a bounded explanation, not a claim that the
/// model understood every tool or that no semantic synonym was missed.
pub fn completeness_estimate(selected: usize, missing: usize) -> f64 {
    let total = selected.saturating_add(missing);
    if total == 0 {
        return 1.0;
    }
    (selected as f64 / total as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn documents() -> Vec<SearchDocument<'static>> {
        vec![
            SearchDocument {
                key: "builtin:read_file",
                name: "read_file",
                description: "Read a UTF-8 file from the worktree.",
                task_kind: "coding",
                category: "files",
            },
            SearchDocument {
                key: "builtin:edit_file",
                name: "edit_file",
                description: "Edit an existing UTF-8 file after reading it.",
                task_kind: "coding",
                category: "files",
            },
            SearchDocument {
                key: "builtin:browser",
                name: "browser",
                description: "Open localhost pages and capture a DOM snapshot.",
                task_kind: "coding",
                category: "browser",
            },
            SearchDocument {
                key: "builtin:search_sources",
                name: "search_sources",
                description: "Search indexed project sources lexically or semantically.",
                task_kind: "research",
                category: "source",
            },
        ]
    }

    #[test]
    fn bm25_ranks_name_then_description_and_is_stable() {
        let docs = documents();
        let first = rank_documents(&docs, "browser snapshot", None, 10);
        assert_eq!(first.first().unwrap().key, "builtin:browser");
        let second = rank_documents(&docs, "browser snapshot", None, 10);
        assert_eq!(first, second);
        let files = rank_documents(&docs, "read file", None, 10);
        assert_eq!(files.first().unwrap().key, "builtin:read_file");
        assert!(files.iter().any(|hit| hit.key == "builtin:edit_file"));
    }

    #[test]
    fn task_kind_boosts_matching_documents() {
        let docs = documents();
        let hits = rank_documents(&docs, "source", Some("research"), 10);
        assert_eq!(hits.first().unwrap().key, "builtin:search_sources");
    }

    #[test]
    fn prerequisite_closure_is_deterministic_and_reports_missing_tools() {
        let available = ["read_file", "edit_file", "team_status"]
            .into_iter()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        let closure = prerequisite_closure(&["edit_file".to_string()], &available, 8);
        assert_eq!(closure.additions.len(), 1);
        assert_eq!(closure.additions[0].name, "read_file");
        assert_eq!(closure.additions[0].required_by, vec!["edit_file"]);
        assert!(closure.missing.is_empty());
        assert_eq!(completeness_estimate(1, 0), 1.0);

        let missing = prerequisite_closure(&["apply_patch".to_string()], &available, 8);
        assert_eq!(missing.additions.len(), 1);
        let unavailable =
            prerequisite_closure(&["search_sources".to_string()], &BTreeSet::new(), 8);
        assert_eq!(unavailable.missing[0].name, "list_sources");
        assert_eq!(unavailable.missing[0].reason, "not_available_for_role");
        assert!(completeness_estimate(1, 1) < 1.0);
    }

    #[test]
    fn empty_query_is_deterministic_and_bounded() {
        let docs = documents();
        let hits = rank_documents(&docs, "", None, 2);
        assert_eq!(hits.len(), 2);
        assert!(hits[0].key < hits[1].key);
        assert!(hits.iter().all(|hit| hit.score == 1.0));
    }
}

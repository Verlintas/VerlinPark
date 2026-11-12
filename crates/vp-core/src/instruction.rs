//! Content-addressed instruction store and prompt layout v3.
//!
//! The cache-first contract is simple: the bytes that precede the
//! conversation must not change unless a stable input changed. Historically
//! the runtime rebuilt the role charter, source brief, skill catalog, run
//! context, and memory brief from generated strings on every call, and it put
//! the *current prompt* into the system prefix. A follow-up prompt therefore
//! invalidated the entire provider prefix.
//!
//! This module owns the deterministic side of the fix:
//!
//! - seven deterministic section keys with fixed ordinals; six are head
//!   system sections, one (`tail/memory`) is a tail section;
//! - a content-addressed blob table (`sha256(text)` -> text) shared across
//!   sessions and agents;
//! - a per-scope ordered state (`session_instruction_state`) that pins the
//!   exact bytes used by an agent so a later generator change cannot silently
//!   move an existing session's prefix;
//! - hash verification on every materialization. A missing or corrupt blob is
//!   rebuilt from the current deterministic generator, stored, and reported as
//!   a bounded `context.instruction.rebuilt` diagnostic. Unverified text is
//!   never sent to a provider;
//! - hard size limits. Oversized sections fail closed with the shared typed
//!   tool-failure contract instead of silently truncating a prompt.
//!
//! This module contains no provider I/O and no hidden model routing. It does
//! not decide whether the provider accepts a mid-history system message; that
//! capability is recorded by the agent request assembly and adapter.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;

use serde_json::Value;
use vp_protocol::types::{Event, InstructionBlob, PromptSectionRef, SessionInstructionState};
use vp_store::Store;

use crate::sha256_hex;
use crate::tooling::errors::{ToolErrorCode, ToolFailure};

/// Legacy combined layout (`core + project + volatile`).
pub const PROMPT_LAYOUT_V2: u32 = 2;
/// Content-addressed ordered layout.
pub const PROMPT_LAYOUT_V3: u32 = 3;

/// Per-section hard ceiling. The role charter + recipes are the largest
/// generated sections in practice; 64 KiB leaves room for unusually long
/// project conventions while still bounding a prompt.
pub const MAX_INSTRUCTION_SECTION_BYTES: usize = 64 * 1024;
/// Aggregate head ceiling. This is intentionally much smaller than a model
/// context window: the instruction prefix must leave room for conversation and
/// output, and an unexpectedly huge source brief should fail closed rather than
/// crowd out the task.
pub const MAX_INSTRUCTION_HEAD_BYTES: usize = 192 * 1024;
/// Aggregate tail ceiling. Memory is explicitly a small, per-run cache miss.
pub const MAX_INSTRUCTION_TAIL_BYTES: usize = 32 * 1024;

/// Normalize a persisted layout value. Unknown values fail closed to the
/// current layout; `2` is preserved only when explicitly selected.
pub fn normalize_prompt_layout(value: u32) -> u32 {
    match value {
        PROMPT_LAYOUT_V2 => PROMPT_LAYOUT_V2,
        _ => PROMPT_LAYOUT_V3,
    }
}

pub fn prompt_layout_name(value: u32) -> &'static str {
    match normalize_prompt_layout(value) {
        PROMPT_LAYOUT_V2 => "v2-combined",
        _ => "v3-instruction-store",
    }
}

/// Deterministic instruction section vocabulary. Ordinals are deliberately
/// spaced by ten so a future section can be inserted without renumbering the
/// existing cache identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum InstructionSectionKey {
    CoreCharter,
    CoreRecipes,
    ProjectSources,
    ProjectSkills,
    SessionContext,
    RunTask,
    TailMemory,
}

impl InstructionSectionKey {
    pub const ALL: [Self; 7] = [
        Self::CoreCharter,
        Self::CoreRecipes,
        Self::ProjectSources,
        Self::ProjectSkills,
        Self::SessionContext,
        Self::RunTask,
        Self::TailMemory,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::CoreCharter => "core/charter",
            Self::CoreRecipes => "core/recipes",
            Self::ProjectSources => "project/sources",
            Self::ProjectSkills => "project/skills",
            Self::SessionContext => "session/context",
            Self::RunTask => "run/task",
            Self::TailMemory => "tail/memory",
        }
    }

    pub fn ordinal(self) -> u32 {
        match self {
            Self::CoreCharter => 10,
            Self::CoreRecipes => 20,
            Self::ProjectSources => 30,
            Self::ProjectSkills => 40,
            Self::SessionContext => 50,
            Self::RunTask => 60,
            Self::TailMemory => 90,
        }
    }

    /// True when the section is sent after the conversation rather than in the
    /// head system prefix.
    pub fn tail(self) -> bool {
        matches!(self, Self::TailMemory)
    }

    /// Parse an exact key. Unknown keys are rejected rather than accepted as a
    /// future cache component, because an unknown component could silently
    /// change the prefix identity.
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.key() == key)
    }

    /// True for sections whose stored bytes define the session/run identity.
    /// Once materialized, these are reused verbatim while the blob verifies;
    /// a later generator change does not rewrite an existing session.
    pub fn pinned(self) -> bool {
        matches!(
            self,
            Self::CoreCharter | Self::CoreRecipes | Self::SessionContext | Self::RunTask
        )
    }

    /// Project sections are semi-stable: a source or skill edit is allowed to
    /// move the project component and therefore the cache epoch. They are
    /// regenerated on each materialization, then stored by content hash.
    pub fn regenerated(self) -> bool {
        matches!(
            self,
            Self::ProjectSources | Self::ProjectSkills | Self::TailMemory
        )
    }

    /// Task-scoped agents only. A Leader follow-up must never gain a `run/task`
    /// section, because that would turn the current prompt back into a head
    /// system message.
    pub fn requires_task_objective(self) -> bool {
        matches!(self, Self::RunTask)
    }

    pub fn max_bytes(self) -> usize {
        match self {
            Self::TailMemory => MAX_INSTRUCTION_TAIL_BYTES,
            _ => MAX_INSTRUCTION_SECTION_BYTES,
        }
    }
}

/// One deterministic candidate for the instruction store. Candidates are
/// sorted by the section's ordinal, never by caller insertion order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionCandidate {
    pub key: InstructionSectionKey,
    pub text: String,
}

impl InstructionCandidate {
    pub fn new(key: InstructionSectionKey, text: impl Into<String>) -> Self {
        Self {
            key,
            text: text.into(),
        }
    }
}

/// One materialized section. `text` is internal prompt material; protocol and
/// event surfaces only expose the hash and metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedSection {
    pub key: InstructionSectionKey,
    pub hash: String,
    pub ordinal: u32,
    pub tail: bool,
    pub text: String,
}

impl MaterializedSection {
    pub fn chars(&self) -> u64 {
        self.text.chars().count() as u64
    }

    pub fn bytes(&self) -> usize {
        self.text.len()
    }

    pub fn reference(&self) -> PromptSectionRef {
        PromptSectionRef::new(self.key.key(), self.hash.clone(), self.ordinal, self.tail)
    }
}

/// Why a referenced blob could not be used verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionRebuildReason {
    MissingBlob,
    HashMismatch,
    MissingState,
}

impl InstructionRebuildReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingBlob => "missing_blob",
            Self::HashMismatch => "hash_mismatch",
            Self::MissingState => "missing_state",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionRebuild {
    pub state_id: String,
    pub key: InstructionSectionKey,
    pub reason: InstructionRebuildReason,
    pub old_hash: Option<String>,
    pub new_hash: String,
    pub chars: u64,
}

impl InstructionRebuild {
    pub fn payload(&self) -> Value {
        serde_json::json!({
            "state_id": self.state_id,
            "key": self.key.key(),
            "reason": self.reason.as_str(),
            "old_hash": self.old_hash,
            "new_hash": self.new_hash,
            "chars": self.chars,
        })
    }
}

/// Result of one materialization pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedInstructions {
    pub layout: u32,
    pub state_id: String,
    pub head: Vec<MaterializedSection>,
    pub tail: Vec<MaterializedSection>,
    pub rebuilds: Vec<InstructionRebuild>,
    /// True when the persisted ordered state changed (new hash, added key,
    /// removed key, or ordinal/tail change).
    pub state_changed: bool,
    /// The complete persisted ordered state for the scope, including a
    /// verified tail that a warm preserved without sending. Missing blobs are
    /// represented by their hash with `available=false` only in the RPC; this
    /// event-facing list is the exact ordered state.
    pub persisted: Vec<PromptSectionRef>,
}

impl MaterializedInstructions {
    pub fn head_refs(&self) -> Vec<PromptSectionRef> {
        self.head
            .iter()
            .map(MaterializedSection::reference)
            .collect()
    }

    pub fn tail_refs(&self) -> Vec<PromptSectionRef> {
        self.tail
            .iter()
            .map(MaterializedSection::reference)
            .collect()
    }

    pub fn head_text(&self) -> String {
        join_section_text(self.head.iter())
    }

    pub fn tail_text(&self) -> String {
        join_section_text(self.tail.iter())
    }

    pub fn head_bytes(&self) -> usize {
        self.head.iter().map(MaterializedSection::bytes).sum()
    }

    pub fn tail_bytes(&self) -> usize {
        self.tail.iter().map(MaterializedSection::bytes).sum()
    }

    pub fn section_chars(&self) -> BTreeMap<String, u64> {
        self.head
            .iter()
            .chain(self.tail.iter())
            .map(|section| (section.key.key().to_string(), section.chars()))
            .collect()
    }

    pub fn section(&self, key: InstructionSectionKey) -> Option<&MaterializedSection> {
        self.head
            .iter()
            .chain(self.tail.iter())
            .find(|section| section.key == key)
    }

    /// Test-only exact-byte inspection. Production callers assemble provider
    /// requests from the same `text` fields but must never expose them through
    /// `session.instructions`.
    #[cfg(test)]
    pub fn exact_text(&self, key: InstructionSectionKey) -> Option<&str> {
        self.section(key).map(|section| section.text.as_str())
    }

    /// The hash of the ordered head prefix text. This is the byte identity the
    /// provider cache reuses; it excludes `tail/memory` by construction.
    pub fn stable_prefix_hash(&self) -> String {
        sha256_hex(self.head_text().as_bytes())
    }

    /// The volatile hash covers every per-run section, including tail memory.
    /// It changes on a memory or task update without changing the head prefix.
    pub fn volatile_hash(&self) -> String {
        let mut material = String::new();
        for section in self.head.iter().filter(|section| {
            matches!(
                section.key,
                InstructionSectionKey::SessionContext | InstructionSectionKey::RunTask
            )
        }) {
            material.push_str(section.key.key());
            material.push('\u{1f}');
            material.push_str(&section.hash);
            material.push('\u{1f}');
        }
        for section in &self.tail {
            material.push_str(section.key.key());
            material.push('\u{1f}');
            material.push_str(&section.hash);
            material.push('\u{1f}');
        }
        sha256_hex(material.as_bytes())
    }

    pub fn head_rows(&self) -> Vec<PromptSectionRef> {
        self.head_refs()
    }

    pub fn tail_rows(&self) -> Vec<PromptSectionRef> {
        self.tail_refs()
    }
}

fn join_section_text<'a>(sections: impl Iterator<Item = &'a MaterializedSection>) -> String {
    let mut rendered = String::new();
    for section in sections {
        if section.text.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(&section.text);
    }
    rendered
}

/// Typed failure for instruction materialization. The shared tool contract is
/// attached so callers can persist a bounded failure object without inventing a
/// second error vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionErrorKind {
    UnknownKey,
    DuplicateKey,
    InvalidLayout,
    OversizeSection,
    OversizeHead,
    OversizeTail,
    HashMismatch,
    MissingBlob,
    StateWriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionError {
    pub kind: InstructionErrorKind,
    pub message: String,
    pub key: Option<String>,
}

impl InstructionError {
    pub fn new(kind: InstructionErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            key: None,
        }
    }

    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn tool_failure(&self) -> ToolFailure {
        let code = match self.kind {
            InstructionErrorKind::OversizeSection
            | InstructionErrorKind::OversizeHead
            | InstructionErrorKind::OversizeTail => ToolErrorCode::RESOURCE_LIMIT,
            InstructionErrorKind::HashMismatch => ToolErrorCode::HASH_MISMATCH,
            InstructionErrorKind::MissingBlob => ToolErrorCode::NOT_FOUND,
            InstructionErrorKind::UnknownKey
            | InstructionErrorKind::DuplicateKey
            | InstructionErrorKind::InvalidLayout => ToolErrorCode::INVALID_ARGUMENT,
            InstructionErrorKind::StateWriteFailed => ToolErrorCode::INTERNAL_ERROR,
        };
        let mut failure = ToolFailure::new(code, self.message.clone())
            .with_tool("instruction_store")
            .with_action("materialize");
        if let Some(key) = &self.key {
            failure = failure.with_evidence(format!("section {key}"));
        }
        failure
    }

    pub fn to_anyhow(&self) -> anyhow::Error {
        anyhow::anyhow!("{self}")
    }
}

impl fmt::Display for InstructionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let failure = self.tool_failure();
        write!(formatter, "error[{}] {}", failure.code, self.message)
    }
}

impl std::error::Error for InstructionError {}

/// Validate the candidate set before touching the store. This is pure and is
/// also used by preview paths that must not persist state.
pub fn validate_candidates(candidates: &[InstructionCandidate]) -> Result<(), InstructionError> {
    let mut seen = BTreeSet::new();
    let mut head_bytes = 0usize;
    let mut tail_bytes = 0usize;
    for candidate in candidates {
        if !seen.insert(candidate.key) {
            return Err(InstructionError::new(
                InstructionErrorKind::DuplicateKey,
                format!(
                    "instruction section {} appears more than once",
                    candidate.key.key()
                ),
            )
            .with_key(candidate.key.key()));
        }
        if candidate.text.len() > candidate.key.max_bytes() {
            return Err(InstructionError::new(
                InstructionErrorKind::OversizeSection,
                format!(
                    "instruction section {} is {} bytes; the limit is {} bytes",
                    candidate.key.key(),
                    candidate.text.len(),
                    candidate.key.max_bytes()
                ),
            )
            .with_key(candidate.key.key()));
        }
        if candidate.key.tail() {
            tail_bytes = tail_bytes.saturating_add(candidate.text.len());
        } else {
            head_bytes = head_bytes.saturating_add(candidate.text.len());
        }
    }
    if head_bytes > MAX_INSTRUCTION_HEAD_BYTES {
        return Err(InstructionError::new(
            InstructionErrorKind::OversizeHead,
            format!(
                "instruction head is {head_bytes} bytes; the limit is {MAX_INSTRUCTION_HEAD_BYTES} bytes"
            ),
        ));
    }
    if tail_bytes > MAX_INSTRUCTION_TAIL_BYTES {
        return Err(InstructionError::new(
            InstructionErrorKind::OversizeTail,
            format!(
                "instruction tail is {tail_bytes} bytes; the limit is {MAX_INSTRUCTION_TAIL_BYTES} bytes"
            ),
        ));
    }
    Ok(())
}

fn sort_candidates(candidates: &mut [InstructionCandidate]) {
    candidates.sort_by_key(|candidate| candidate.key.ordinal());
}

/// The canonical content hash for one instruction section. Store writers and
/// archive importers use the same helper so a bundle can verify a blob without
/// reconstructing prompt text.
pub fn hash_instruction_text(text: &str) -> String {
    sha256_hex(text.as_bytes())
}

fn verify_blob(blob: &InstructionBlob) -> bool {
    blob.hash.len() == 64
        && blob.hash.chars().all(|value| value.is_ascii_hexdigit())
        && sha256_hex(blob.text.as_bytes()) == blob.hash
}

/// Build an in-memory preview without touching the store. Used by
/// `session.cache.plan`, which must remain a read-only preview and must never
/// create a provider call, worktree, or state row.
pub fn preview(
    state_id: &str,
    candidates: &[InstructionCandidate],
    include_tail: bool,
) -> Result<MaterializedInstructions, InstructionError> {
    let mut sorted = candidates
        .iter()
        .filter(|candidate| include_tail || !candidate.key.tail())
        .cloned()
        .collect::<Vec<_>>();
    validate_candidates(&sorted)?;
    sort_candidates(&mut sorted);
    let sections = sorted
        .into_iter()
        .map(|candidate| MaterializedSection {
            key: candidate.key,
            hash: sha256_hex(candidate.text.as_bytes()),
            ordinal: candidate.key.ordinal(),
            tail: candidate.key.tail(),
            text: candidate.text,
        })
        .collect::<Vec<_>>();
    let head = sections
        .iter()
        .filter(|section| !section.tail)
        .cloned()
        .collect::<Vec<_>>();
    let tail = sections
        .iter()
        .filter(|section| section.tail)
        .cloned()
        .collect::<Vec<_>>();
    let persisted = sections
        .iter()
        .map(MaterializedSection::reference)
        .collect::<Vec<_>>();
    Ok(MaterializedInstructions {
        layout: PROMPT_LAYOUT_V3,
        state_id: state_id.to_string(),
        head,
        tail,
        rebuilds: Vec::new(),
        state_changed: false,
        persisted,
    })
}

/// Materialize one instruction scope from content-addressed blobs.
///
/// Rules:
/// - pinned sections reuse verified stored bytes even when a freshly generated
///   candidate would differ;
/// - project sections and tail memory use the freshly generated candidate and
///   store a new blob when the hash changed;
/// - a referenced blob that is missing or fails its hash is rebuilt from the
///   candidate and reported in `rebuilds`;
/// - the ordered state is written atomically only when it changed.
pub fn materialize(
    store: &Store,
    state_id: &str,
    candidates: &[InstructionCandidate],
    include_tail: bool,
) -> Result<MaterializedInstructions, InstructionError> {
    if state_id.trim().is_empty() {
        return Err(InstructionError::new(
            InstructionErrorKind::InvalidLayout,
            "instruction state id must not be empty",
        ));
    }
    let mut sorted = candidates
        .iter()
        .filter(|candidate| include_tail || !candidate.key.tail())
        .cloned()
        .collect::<Vec<_>>();
    validate_candidates(&sorted)?;
    sort_candidates(&mut sorted);

    let existing_rows = store.list_session_instructions(state_id).map_err(|error| {
        InstructionError::new(
            InstructionErrorKind::StateWriteFailed,
            format!("could not read instruction state: {error:#}"),
        )
    })?;
    let existing_by_key = existing_rows
        .iter()
        .map(|row| (row.key.clone(), row.clone()))
        .collect::<HashMap<_, _>>();
    let had_state = !existing_rows.is_empty();

    let mut sections = Vec::with_capacity(sorted.len());
    let mut rebuilds = Vec::new();
    let now = vp_protocol::now_rfc3339();

    for candidate in &sorted {
        let desired_hash = sha256_hex(candidate.text.as_bytes());
        let existing = existing_by_key.get(candidate.key.key());
        let mut chosen_text = None;
        let mut chosen_hash = desired_hash.clone();
        let mut rebuilt_reason = None;
        let mut old_hash = None;

        if let Some(existing) = existing {
            old_hash = Some(existing.hash.clone());
            if candidate.key.pinned() {
                match store.get_instruction_blob(&existing.hash) {
                    Ok(Some(blob)) if verify_blob(&blob) => {
                        chosen_text = Some(blob.text);
                        chosen_hash = existing.hash.clone();
                    }
                    Ok(Some(_)) => {
                        rebuilt_reason = Some(InstructionRebuildReason::HashMismatch);
                    }
                    Ok(None) => {
                        rebuilt_reason = Some(InstructionRebuildReason::MissingBlob);
                    }
                    Err(error) => {
                        return Err(InstructionError::new(
                            InstructionErrorKind::StateWriteFailed,
                            format!("could not read instruction blob: {error:#}"),
                        )
                        .with_key(candidate.key.key()));
                    }
                }
            } else if existing.hash == desired_hash {
                // Project/memory content is regenerated, but when the hash is
                // unchanged there is no write and no byte movement.
                chosen_text = Some(candidate.text.clone());
                chosen_hash = desired_hash.clone();
            } else {
                // Semi-stable content legitimately changed. It remains a real
                // cache-epoch change, recorded by the state event.
                chosen_text = Some(candidate.text.clone());
                chosen_hash = desired_hash.clone();
            }
        } else {
            if had_state && candidate.key.pinned() {
                rebuilt_reason = Some(InstructionRebuildReason::MissingState);
            }
            chosen_text = Some(candidate.text.clone());
            chosen_hash = desired_hash.clone();
        }

        let source_text = chosen_text.unwrap_or_else(|| candidate.text.clone());
        // Never send unverified bytes. A candidate is generated in-process, but
        // this final check also covers a future caller that supplies text from
        // an external source.
        if sha256_hex(source_text.as_bytes()) != chosen_hash {
            return Err(InstructionError::new(
                InstructionErrorKind::HashMismatch,
                format!(
                    "instruction section {} hash does not match its text",
                    candidate.key.key()
                ),
            )
            .with_key(candidate.key.key()));
        }
        if store.put_instruction_blob(&source_text).is_err() {
            return Err(InstructionError::new(
                InstructionErrorKind::StateWriteFailed,
                format!(
                    "could not store instruction blob for {}",
                    candidate.key.key()
                ),
            )
            .with_key(candidate.key.key()));
        }
        // Materialize only from the store after the write, so a corrupted body
        // is read back through the same verification path as a normal hit.
        let stored = match store.get_instruction_blob(&chosen_hash) {
            Ok(Some(blob)) if verify_blob(&blob) => blob,
            Ok(Some(_)) => {
                return Err(InstructionError::new(
                    InstructionErrorKind::HashMismatch,
                    format!(
                        "instruction section {} blob failed verification after write",
                        candidate.key.key()
                    ),
                )
                .with_key(candidate.key.key()));
            }
            Ok(None) => {
                return Err(InstructionError::new(
                    InstructionErrorKind::MissingBlob,
                    format!(
                        "instruction section {} blob is missing after write",
                        candidate.key.key()
                    ),
                )
                .with_key(candidate.key.key()));
            }
            Err(error) => {
                return Err(InstructionError::new(
                    InstructionErrorKind::StateWriteFailed,
                    format!("could not read instruction blob: {error:#}"),
                )
                .with_key(candidate.key.key()));
            }
        };
        let text = stored.text;
        if text.len() > candidate.key.max_bytes() {
            return Err(InstructionError::new(
                InstructionErrorKind::OversizeSection,
                format!(
                    "instruction section {} is {} bytes; the limit is {} bytes",
                    candidate.key.key(),
                    text.len(),
                    candidate.key.max_bytes()
                ),
            )
            .with_key(candidate.key.key()));
        }

        if let Some(reason) = rebuilt_reason {
            rebuilds.push(InstructionRebuild {
                state_id: state_id.to_string(),
                key: candidate.key,
                reason,
                old_hash,
                new_hash: chosen_hash.clone(),
                chars: text.chars().count() as u64,
            });
        }

        sections.push(MaterializedSection {
            key: candidate.key,
            hash: chosen_hash,
            ordinal: candidate.key.ordinal(),
            tail: candidate.key.tail(),
            text,
        });
    }

    // Warm requests intentionally exclude memory. Preserve verified tail rows
    // so a warm does not erase the last real run's memory state.
    if !include_tail {
        for row in existing_rows.iter().filter(|row| row.tail) {
            if sorted
                .iter()
                .any(|candidate| candidate.key.key() == row.key)
            {
                continue;
            }
            let Ok(Some(blob)) = store.get_instruction_blob(&row.hash) else {
                continue;
            };
            if !verify_blob(&blob) {
                continue;
            }
            if let Some(key) = InstructionSectionKey::parse(&row.key) {
                sections.push(MaterializedSection {
                    key,
                    hash: row.hash.clone(),
                    ordinal: row.ordinal,
                    tail: row.tail,
                    text: blob.text,
                });
            }
        }
        sections.sort_by_key(|section| section.ordinal);
    }

    let head_bytes = sections
        .iter()
        .filter(|section| !section.tail)
        .map(MaterializedSection::bytes)
        .sum::<usize>();
    let tail_bytes = sections
        .iter()
        .filter(|section| section.tail)
        .map(MaterializedSection::bytes)
        .sum::<usize>();
    if head_bytes > MAX_INSTRUCTION_HEAD_BYTES {
        return Err(InstructionError::new(
            InstructionErrorKind::OversizeHead,
            format!(
                "materialized instruction head is {head_bytes} bytes; the limit is {MAX_INSTRUCTION_HEAD_BYTES} bytes"
            ),
        ));
    }
    if tail_bytes > MAX_INSTRUCTION_TAIL_BYTES {
        return Err(InstructionError::new(
            InstructionErrorKind::OversizeTail,
            format!(
                "materialized instruction tail is {tail_bytes} bytes; the limit is {MAX_INSTRUCTION_TAIL_BYTES} bytes"
            ),
        ));
    }

    let rows = sections
        .iter()
        .map(|section| SessionInstructionState {
            session_id: state_id.to_string(),
            key: section.key.key().to_string(),
            hash: section.hash.clone(),
            ordinal: section.ordinal,
            tail: section.tail,
            updated_at: now.clone(),
        })
        .collect::<Vec<_>>();
    let state_changed = !same_rows(&existing_rows, &rows);
    if state_changed {
        store
            .replace_session_instructions(state_id, &rows)
            .map_err(|error| {
                InstructionError::new(
                    InstructionErrorKind::StateWriteFailed,
                    format!("could not persist instruction state: {error:#}"),
                )
            })?;
    }

    let head = sections
        .iter()
        .filter(|section| !section.tail)
        .cloned()
        .collect::<Vec<_>>();
    // A warm materializes the head and preserves verified tail rows for the
    // next real run, but it must not send memory as part of the stable prefix.
    let tail = if include_tail {
        sections
            .iter()
            .filter(|section| section.tail)
            .cloned()
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let persisted = rows
        .iter()
        .map(|row| PromptSectionRef::new(row.key.clone(), row.hash.clone(), row.ordinal, row.tail))
        .collect::<Vec<_>>();
    Ok(MaterializedInstructions {
        layout: PROMPT_LAYOUT_V3,
        state_id: state_id.to_string(),
        head,
        tail,
        rebuilds,
        state_changed,
        persisted,
    })
}

fn same_rows(left: &[SessionInstructionState], right: &[SessionInstructionState]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let left = left
        .iter()
        .map(|row| (&row.key, (&row.hash, row.ordinal, row.tail)))
        .collect::<BTreeMap<_, _>>();
    let right = right
        .iter()
        .map(|row| (&row.key, (&row.hash, row.ordinal, row.tail)))
        .collect::<BTreeMap<_, _>>();
    left == right
}

/// Compare projected and replayed rows. Timestamps are deliberately excluded:
/// a repair writes the event time, while an in-flight materialization may have
/// a newer local timestamp with identical ordered bytes.
pub fn instruction_rows_match(
    left: &[SessionInstructionState],
    right: &[SessionInstructionState],
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let strip = |rows: &[SessionInstructionState]| {
        rows.iter()
            .map(|row| {
                (
                    row.session_id.clone(),
                    row.key.clone(),
                    row.hash.clone(),
                    row.ordinal,
                    row.tail,
                )
            })
            .collect::<Vec<_>>()
    };
    let mut left = strip(left);
    let mut right = strip(right);
    left.sort();
    right.sort();
    left == right
}

/// Reconstruct instruction state rows from `context.instruction` events.
///
/// Each event carries the complete ordered state for one scope. The last event
/// per scope wins. A malformed payload returns `None` so replay reports drift
/// instead of silently dropping a section.
pub fn states_from_events(events: &[Event]) -> Option<Vec<SessionInstructionState>> {
    let mut latest: BTreeMap<String, Vec<SessionInstructionState>> = BTreeMap::new();
    for event in events
        .iter()
        .filter(|event| event.kind == "context.instruction")
    {
        let state_id = event.payload.get("state_id")?.as_str()?.to_string();
        let sections = event.payload.get("sections")?.as_array()?;
        let mut rows = Vec::with_capacity(sections.len());
        let mut seen = BTreeSet::new();
        for section in sections {
            let key = section.get("key")?.as_str()?;
            let parsed = InstructionSectionKey::parse(key)?;
            if !seen.insert(parsed) {
                return None;
            }
            let hash = section.get("hash")?.as_str()?;
            if hash.len() != 64 || !hash.chars().all(|value| value.is_ascii_hexdigit()) {
                return None;
            }
            let ordinal = section.get("ordinal")?.as_u64()? as u32;
            let tail = section.get("tail")?.as_bool()?;
            // The ordinal and tail flag are part of the cache identity. Reject
            // a drifted event instead of silently sorting it into a new order.
            if ordinal != parsed.ordinal() || tail != parsed.tail() {
                return None;
            }
            rows.push(SessionInstructionState {
                session_id: state_id.clone(),
                key: key.to_string(),
                hash: hash.to_string(),
                ordinal,
                tail,
                updated_at: event.created_at.clone(),
            });
        }
        rows.sort_by_key(|row| row.ordinal);
        latest.insert(state_id, rows);
    }
    if latest.is_empty() {
        return None;
    }
    let mut rows = latest.into_values().flatten().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| left.key.cmp(&right.key))
    });
    Some(rows)
}

/// Latest `context.prompt_prefix` identity for the instructions RPC:
/// `(prefix_hash, cache_epoch, stable_prefix_hash)`.
pub fn latest_prompt_prefix(events: &[Event]) -> (Option<String>, Option<String>, Option<String>) {
    let latest = events
        .iter()
        .rev()
        .find(|event| event.kind == "context.prompt_prefix");
    match latest {
        Some(event) => {
            let finalized_epoch = event
                .payload
                .get("cache_epoch_placeholder")
                .and_then(Value::as_bool)
                != Some(true)
                && event
                    .payload
                    .get("cache_epoch_source")
                    .and_then(Value::as_str)
                    != Some("l1_placeholder");
            (
                event
                    .payload
                    .get("prefix_hash")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                event
                    .payload
                    .get("cache_epoch")
                    .and_then(Value::as_str)
                    .filter(|_| finalized_epoch)
                    .map(str::to_string),
                event
                    .payload
                    .get("stable_prefix_hash")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        }
        None => (None, None, None),
    }
}

/// True when a provider safely accepts a system message at the tail of the
/// message array. Anthropic uses a top-level `system` field and the adapter
/// falls back to a stable `[memory]` user message; OpenAI-compatible endpoints
/// accept a trailing system message. This is a deterministic provider contract,
/// not a probe and not an extra provider call.
pub fn provider_accepts_tail_system(provider: &str) -> bool {
    !matches!(provider.trim().to_ascii_lowercase().as_str(), "anthropic")
}

/// Stable scope id for one agent. The Leader uses the session id, preserving
/// the plain `session_instruction_state(session_id, key)` contract. Child
/// agents use `<session>#<agent>` so their role charter and task objective stay
/// isolated while keys and ordinals remain identical.
pub fn instruction_state_id(session_id: &str, agent_id: &str, leader: bool) -> String {
    if leader {
        session_id.to_string()
    } else {
        format!("{session_id}#{agent_id}")
    }
}

/// Human-readable role/agent metadata for the instructions RPC.
pub fn instruction_state_agent(state_id: &str, session_id: &str) -> Option<String> {
    state_id
        .strip_prefix(session_id)
        .and_then(|rest| rest.strip_prefix('#'))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn candidates() -> Vec<InstructionCandidate> {
        vec![
            InstructionCandidate::new(InstructionSectionKey::CoreCharter, "charter v1"),
            InstructionCandidate::new(InstructionSectionKey::CoreRecipes, "recipes v1"),
            InstructionCandidate::new(InstructionSectionKey::ProjectSources, "sources v1"),
            InstructionCandidate::new(InstructionSectionKey::ProjectSkills, "skills v1"),
            InstructionCandidate::new(InstructionSectionKey::SessionContext, "session context v1"),
            InstructionCandidate::new(InstructionSectionKey::RunTask, "task objective v1"),
            InstructionCandidate::new(InstructionSectionKey::TailMemory, "memory v1"),
        ]
    }

    #[test]
    fn latest_prompt_prefix_ignores_l1_placeholder_epochs() {
        let placeholder = Event {
            session_id: "ses".into(),
            seq: 1,
            kind: "context.prompt_prefix".into(),
            payload: serde_json::json!({
                "prefix_hash": "a".repeat(64),
                "cache_epoch": "a".repeat(64),
                "cache_epoch_source": "l1_placeholder",
                "cache_epoch_placeholder": true,
                "stable_prefix_hash": "b".repeat(64),
            }),
            created_at: "2026-11-12T00:00:01Z".into(),
        };
        let (prefix, epoch, stable) = latest_prompt_prefix(&[placeholder]);
        assert_eq!(prefix, Some("a".repeat(64)));
        assert!(epoch.is_none());
        assert_eq!(stable, Some("b".repeat(64)));

        let finalized = Event {
            session_id: "ses".into(),
            seq: 2,
            kind: "context.prompt_prefix".into(),
            payload: serde_json::json!({
                "prefix_hash": "c".repeat(64),
                "cache_epoch": "d".repeat(64),
                "cache_epoch_source": "l4",
                "cache_epoch_placeholder": false,
            }),
            created_at: "2026-11-12T00:00:02Z".into(),
        };
        let (_, epoch, _) = latest_prompt_prefix(&[finalized]);
        assert_eq!(epoch, Some("d".repeat(64)));
    }

    #[test]
    fn key_vocabulary_is_unique_ordered_and_tail_is_last() {
        let mut ordinals = Vec::new();
        for key in InstructionSectionKey::ALL {
            assert_eq!(InstructionSectionKey::parse(key.key()), Some(key));
            assert!(!key.key().is_empty());
            ordinals.push(key.ordinal());
        }
        let mut sorted = ordinals.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ordinals, sorted, "ordinals must be declared in order");
        assert_eq!(InstructionSectionKey::TailMemory.ordinal(), 90);
        assert!(InstructionSectionKey::TailMemory.tail());
        for key in InstructionSectionKey::ALL {
            if key != InstructionSectionKey::TailMemory {
                assert!(!key.tail(), "{} must be a head section", key.key());
            }
        }
    }

    #[test]
    fn layout_normalization_keeps_explicit_v2_and_defaults_unknown_to_v3() {
        assert_eq!(normalize_prompt_layout(2), 2);
        assert_eq!(normalize_prompt_layout(3), 3);
        assert_eq!(normalize_prompt_layout(0), 3);
        assert_eq!(normalize_prompt_layout(99), 3);
        assert_eq!(prompt_layout_name(2), "v2-combined");
        assert_eq!(prompt_layout_name(3), "v3-instruction-store");
    }

    #[test]
    fn provider_tail_capability_is_explicit() {
        assert!(provider_accepts_tail_system("openai-compatible"));
        assert!(provider_accepts_tail_system("deepseek"));
        assert!(!provider_accepts_tail_system("anthropic"));
        assert!(!provider_accepts_tail_system(" Anthropic "));
    }

    #[test]
    fn scope_ids_are_deterministic_and_isolated() {
        assert_eq!(
            instruction_state_id("ses-1", "leader-1", true),
            "ses-1".to_string()
        );
        assert_eq!(
            instruction_state_id("ses-1", "worker-9", false),
            "ses-1#worker-9"
        );
        assert_eq!(
            instruction_state_agent("ses-1#worker-9", "ses-1"),
            Some("worker-9".to_string())
        );
        assert_eq!(instruction_state_agent("ses-1", "ses-1"), None);
    }

    #[test]
    fn materialization_is_byte_identical_across_two_calls() {
        let (_dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let second = materialize(&store, "ses-1", &candidates(), true).unwrap();
        assert_eq!(first.head_text(), second.head_text());
        assert_eq!(first.tail_text(), second.tail_text());
        assert_eq!(first.stable_prefix_hash(), second.stable_prefix_hash());
        assert!(!second.state_changed);
        assert!(second.rebuilds.is_empty());
        for key in [
            InstructionSectionKey::CoreCharter,
            InstructionSectionKey::CoreRecipes,
            InstructionSectionKey::ProjectSources,
            InstructionSectionKey::ProjectSkills,
            InstructionSectionKey::SessionContext,
            InstructionSectionKey::RunTask,
            InstructionSectionKey::TailMemory,
        ] {
            assert_eq!(first.exact_text(key), second.exact_text(key), "{key:?}");
        }
    }

    #[test]
    fn a_memory_change_moves_only_the_tail_and_not_the_head_prefix() {
        let (_dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let mut changed = candidates();
        changed
            .iter_mut()
            .find(|candidate| candidate.key == InstructionSectionKey::TailMemory)
            .unwrap()
            .text = "memory v2: the API key lives in 1Password".to_string();
        let second = materialize(&store, "ses-1", &changed, true).unwrap();
        assert_eq!(first.head_text(), second.head_text());
        assert_eq!(first.stable_prefix_hash(), second.stable_prefix_hash());
        assert_ne!(first.tail_text(), second.tail_text());
        assert_ne!(first.volatile_hash(), second.volatile_hash());
        assert!(
            first
                .section(InstructionSectionKey::TailMemory)
                .unwrap()
                .tail
        );
    }

    #[test]
    fn project_sections_are_regenerated_but_pinned_sections_stay_stable() {
        let (_dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let mut changed = candidates();
        for candidate in &mut changed {
            match candidate.key {
                InstructionSectionKey::ProjectSources => candidate.text = "sources v2".to_string(),
                InstructionSectionKey::SessionContext => {
                    candidate.text = "a newer generator value".to_string()
                }
                _ => {}
            }
        }
        let second = materialize(&store, "ses-1", &changed, true).unwrap();
        assert_eq!(
            first.exact_text(InstructionSectionKey::SessionContext),
            second.exact_text(InstructionSectionKey::SessionContext)
        );
        assert_ne!(
            first.exact_text(InstructionSectionKey::ProjectSources),
            second.exact_text(InstructionSectionKey::ProjectSources)
        );
        assert_ne!(first.stable_prefix_hash(), second.stable_prefix_hash());
        assert!(second.rebuilds.is_empty());
    }

    #[test]
    fn missing_blob_rebuilds_and_reports_a_bounded_diagnostic() {
        let (_dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let hash = first
            .section(InstructionSectionKey::CoreCharter)
            .unwrap()
            .hash
            .clone();
        assert!(store.delete_instruction_blob(&hash).unwrap());
        let second = materialize(&store, "ses-1", &candidates(), true).unwrap();
        // Rebuilding the same deterministic text restores the original hash,
        // so the ordered state is unchanged; the diagnostic still records the
        // repair instead of silently accepting missing bytes.
        assert!(!second.state_changed);
        assert_eq!(second.rebuilds.len(), 1);
        assert_eq!(
            second.rebuilds[0].reason,
            InstructionRebuildReason::MissingBlob
        );
        assert_eq!(second.rebuilds[0].key, InstructionSectionKey::CoreCharter);
        assert_eq!(second.rebuilds[0].old_hash.as_deref(), Some(hash.as_str()));
        assert_eq!(
            second.exact_text(InstructionSectionKey::CoreCharter),
            Some("charter v1")
        );
        let blob = store
            .get_instruction_blob(&second.rebuilds[0].new_hash)
            .unwrap()
            .unwrap();
        assert_eq!(blob.text, "charter v1");
    }

    #[test]
    fn corrupt_blob_rebuilds_deterministically_and_reports_hash_mismatch() {
        let (dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let hash = first
            .section(InstructionSectionKey::CoreCharter)
            .unwrap()
            .hash
            .clone();
        // Simulate a partially corrupted store. The materializer must never
        // use this text; it verifies the hash, rebuilds from the deterministic
        // candidate, and reports the repair.
        let conn = rusqlite::Connection::open(dir.path().join("verlinpark.db")).unwrap();
        conn.execute(
            "UPDATE instruction_blobs SET text = 'corrupt bytes' WHERE hash = ?1",
            rusqlite::params![hash],
        )
        .unwrap();
        drop(conn);
        let second = materialize(&store, "ses-1", &candidates(), true).unwrap();
        assert_eq!(second.rebuilds.len(), 1);
        assert_eq!(
            second.rebuilds[0].reason,
            InstructionRebuildReason::HashMismatch
        );
        assert_eq!(
            second.exact_text(InstructionSectionKey::CoreCharter),
            Some("charter v1")
        );
        let repaired = store.get_instruction_blob(&hash).unwrap().unwrap();
        assert_eq!(repaired.text, "charter v1");
    }

    #[test]
    fn hash_verification_rejects_a_mismatched_blob() {
        let blob = InstructionBlob {
            hash: "0".repeat(64),
            text: "not the hash".to_string(),
            created_at: "now".to_string(),
        };
        assert!(!verify_blob(&blob));
        let valid = InstructionBlob {
            hash: sha256_hex(b"charter"),
            text: "charter".to_string(),
            created_at: "now".to_string(),
        };
        assert!(verify_blob(&valid));
    }

    #[test]
    fn oversized_section_fails_closed_without_writing_state() {
        let (_dir, store) = store();
        let huge = "x".repeat(MAX_INSTRUCTION_SECTION_BYTES + 1);
        let candidates = vec![InstructionCandidate::new(
            InstructionSectionKey::CoreCharter,
            huge,
        )];
        let error = materialize(&store, "ses-1", &candidates, true).unwrap_err();
        assert_eq!(error.kind, InstructionErrorKind::OversizeSection);
        assert_eq!(error.tool_failure().code, ToolErrorCode::RESOURCE_LIMIT);
        assert!(store.list_session_instructions("ses-1").unwrap().is_empty());
    }

    #[test]
    fn oversized_total_head_fails_closed() {
        let mut candidates = Vec::new();
        let chunk = "y".repeat(MAX_INSTRUCTION_SECTION_BYTES - 10);
        for key in [
            InstructionSectionKey::CoreCharter,
            InstructionSectionKey::CoreRecipes,
            InstructionSectionKey::ProjectSources,
            InstructionSectionKey::ProjectSkills,
        ] {
            candidates.push(InstructionCandidate::new(key, chunk.clone()));
        }
        let error = validate_candidates(&candidates).unwrap_err();
        assert_eq!(error.kind, InstructionErrorKind::OversizeHead);
        assert_eq!(error.tool_failure().code, ToolErrorCode::RESOURCE_LIMIT);
    }

    #[test]
    fn duplicate_and_unknown_keys_fail_closed() {
        let duplicate = vec![
            InstructionCandidate::new(InstructionSectionKey::CoreCharter, "a"),
            InstructionCandidate::new(InstructionSectionKey::CoreCharter, "b"),
        ];
        assert_eq!(
            validate_candidates(&duplicate).unwrap_err().kind,
            InstructionErrorKind::DuplicateKey
        );
        assert_eq!(InstructionSectionKey::parse("core/unknown"), None);
    }

    #[test]
    fn warm_without_memory_preserves_the_verified_tail_state() {
        let (_dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let head_only = candidates()
            .into_iter()
            .filter(|candidate| !candidate.key.tail())
            .collect::<Vec<_>>();
        let warm = materialize(&store, "ses-1", &head_only, false).unwrap();
        assert_eq!(warm.head_text(), first.head_text());
        assert!(warm.tail.is_empty());
        let rows = store.list_session_instructions("ses-1").unwrap();
        assert!(rows.iter().any(|row| row.key == "tail/memory"));
    }

    #[test]
    fn replay_reconstructs_the_ordered_state_and_rejects_drift() {
        let (_dir, store) = store();
        let materialized = materialize(&store, "ses-1", &candidates(), true).unwrap();
        let rows = store.list_session_instructions("ses-1").unwrap();
        let event = Event {
            session_id: "ses-1".to_string(),
            seq: 1,
            kind: "context.instruction".to_string(),
            payload: serde_json::json!({
                "state_id": "ses-1",
                "layout": 3,
                "sections": rows.iter().map(|row| serde_json::json!({
                    "key": row.key,
                    "hash": row.hash,
                    "ordinal": row.ordinal,
                    "tail": row.tail,
                })).collect::<Vec<_>>(),
            }),
            created_at: "2026-11-12T00:00:00Z".to_string(),
        };
        let replay = states_from_events(std::slice::from_ref(&event)).unwrap();
        assert!(instruction_rows_match(&rows, &replay));
        assert_eq!(
            replay.iter().map(|row| row.ordinal).collect::<Vec<_>>(),
            vec![10, 20, 30, 40, 50, 60, 90]
        );
        let mut drifted = event.clone();
        drifted.payload["sections"][0]["ordinal"] = serde_json::json!(99);
        assert!(states_from_events(&[drifted]).is_none());
        assert_eq!(materialized.layout, PROMPT_LAYOUT_V3);
    }

    #[test]
    fn reconstruction_without_events_is_none_not_a_fabricated_state() {
        assert!(states_from_events(&[]).is_none());
    }

    #[test]
    fn section_references_never_contain_text() {
        let (_dir, store) = store();
        let materialized = materialize(&store, "ses-1", &candidates(), true).unwrap();
        for reference in materialized
            .head_refs()
            .into_iter()
            .chain(materialized.tail_refs())
        {
            let encoded = serde_json::to_string(&reference).unwrap();
            assert!(!encoded.contains("charter v1"));
            assert!(!encoded.contains("memory v1"));
            assert_eq!(reference.hash.len(), 64);
        }
    }
}

#[cfg(test)]
mod edge_tests {
    use super::*;
    use tempfile::TempDir;

    fn store() -> (TempDir, Store) {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, store)
    }

    fn candidates(memory: &str) -> Vec<InstructionCandidate> {
        vec![
            InstructionCandidate::new(InstructionSectionKey::CoreCharter, "charter"),
            InstructionCandidate::new(InstructionSectionKey::SessionContext, "session"),
            InstructionCandidate::new(InstructionSectionKey::TailMemory, memory),
        ]
    }

    #[test]
    fn preview_never_writes_blobs_or_state() {
        let (_dir, store) = store();
        let preview = super::preview("ses-1", &candidates("memory"), true).unwrap();
        assert_eq!(preview.head.len(), 2);
        assert_eq!(preview.tail.len(), 1);
        assert!(store.list_session_instructions("ses-1").unwrap().is_empty());
        assert!(
            store
                .get_instruction_blob(&preview.head[0].hash)
                .unwrap()
                .is_none()
        );
        assert!(!preview.state_changed);
        assert!(preview.rebuilds.is_empty());
    }

    #[test]
    fn scoped_states_are_isolated_and_keep_independent_tails() {
        let (_dir, store) = store();
        let leader = materialize(&store, "ses-1", &candidates("leader memory"), true).unwrap();
        let worker =
            materialize(&store, "ses-1#worker-1", &candidates("worker memory"), true).unwrap();
        assert_eq!(leader.head_text(), worker.head_text());
        assert_ne!(leader.tail_text(), worker.tail_text());
        assert_eq!(store.list_session_instructions("ses-1").unwrap().len(), 3);
        assert_eq!(
            store
                .list_session_instructions("ses-1#worker-1")
                .unwrap()
                .len(),
            3
        );
        let all = store.list_instruction_states_for_session("ses-1").unwrap();
        assert_eq!(all.len(), 6);
        assert_eq!(
            all.iter()
                .filter(|row| row.session_id == "ses-1#worker-1")
                .count(),
            3
        );
    }

    #[test]
    fn empty_state_id_fails_closed() {
        let (_dir, store) = store();
        let error = materialize(&store, "  ", &candidates("memory"), true).unwrap_err();
        assert_eq!(error.kind, InstructionErrorKind::InvalidLayout);
    }

    #[test]
    fn oversized_tail_fails_closed_before_any_store_write() {
        let (_dir, store) = store();
        let huge = "z".repeat(MAX_INSTRUCTION_TAIL_BYTES + 1);
        let error = materialize(&store, "ses-1", &candidates(&huge), true).unwrap_err();
        assert_eq!(error.kind, InstructionErrorKind::OversizeSection);
        assert!(store.list_session_instructions("ses-1").unwrap().is_empty());
    }

    #[test]
    fn replay_uses_the_latest_complete_state_per_scope() {
        let (_dir, store) = store();
        let first = materialize(&store, "ses-1", &candidates("one"), true).unwrap();
        let second = materialize(&store, "ses-1", &candidates("two"), true).unwrap();
        let state_event = |seq: u64, rows: &[PromptSectionRef], created_at: &str| Event {
            session_id: "ses-1".to_string(),
            seq,
            kind: "context.instruction".to_string(),
            payload: serde_json::json!({
                "state_id": "ses-1",
                "layout": 3,
                "sections": rows.iter().map(|row| serde_json::json!({
                    "key": row.key,
                    "hash": row.hash,
                    "ordinal": row.ordinal,
                    "tail": row.tail,
                })).collect::<Vec<_>>(),
            }),
            created_at: created_at.to_string(),
        };
        let first_rows = first
            .head_refs()
            .into_iter()
            .chain(first.tail_refs())
            .collect::<Vec<_>>();
        let second_rows = second
            .head_refs()
            .into_iter()
            .chain(second.tail_refs())
            .collect::<Vec<_>>();
        let events = vec![
            state_event(1, &first_rows, "2026-11-12T00:00:00Z"),
            state_event(2, &second_rows, "2026-11-12T00:00:01Z"),
        ];
        let replayed = states_from_events(&events).unwrap();
        let projected = store.list_session_instructions("ses-1").unwrap();
        assert!(instruction_rows_match(&projected, &replayed));
        assert_eq!(
            replayed
                .iter()
                .find(|row| row.key == "tail/memory")
                .map(|row| row.hash.as_str()),
            projected
                .iter()
                .find(|row| row.key == "tail/memory")
                .map(|row| row.hash.as_str())
        );
    }

    #[test]
    fn malformed_replay_is_none_not_a_partial_state() {
        let event = Event {
            session_id: "ses-1".to_string(),
            seq: 1,
            kind: "context.instruction".to_string(),
            payload: serde_json::json!({
                "state_id": "ses-1",
                "sections": [{"key": "core/charter", "hash": "short", "ordinal": 10, "tail": false}]
            }),
            created_at: "2026-11-12T00:00:00Z".to_string(),
        };
        assert!(states_from_events(&[event]).is_none());
    }

    #[test]
    fn missing_state_diagnostic_is_emitted_only_for_a_partial_existing_scope() {
        let (_dir, store) = store();
        // Create a state with only one section.
        let only_session = vec![InstructionCandidate::new(
            InstructionSectionKey::SessionContext,
            "session",
        )];
        materialize(&store, "ses-1", &only_session, true).unwrap();
        // A later materialization expects core/charter but the scope already
        // has state, so the missing key is reported instead of silently
        // appending a new head section.
        let second = materialize(&store, "ses-1", &candidates("memory"), true).unwrap();
        assert!(
            second
                .rebuilds
                .iter()
                .any(|rebuild| rebuild.key == InstructionSectionKey::CoreCharter
                    && rebuild.reason == InstructionRebuildReason::MissingState)
        );
    }

    #[test]
    fn rebuilt_diagnostic_payload_is_bounded_and_contains_no_text() {
        let diagnostic = InstructionRebuild {
            state_id: "ses-1".to_string(),
            key: InstructionSectionKey::CoreCharter,
            reason: InstructionRebuildReason::HashMismatch,
            old_hash: Some("a".repeat(64)),
            new_hash: "b".repeat(64),
            chars: 42,
        };
        let payload = diagnostic.payload().to_string();
        assert!(payload.contains("hash_mismatch"));
        assert!(payload.contains("core/charter"));
        assert!(!payload.contains("roles"));
        assert!(!payload.contains("memory"));
        assert!(payload.len() < 512);
    }
}

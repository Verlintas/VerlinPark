//! Cache-first prompt assembly and attribution.
//!
//! The runtime splits the system prompt into three deterministic sections:
//!
//! - `core`: role charter, security/output contract, and stable project
//!   conventions. Shared by every agent with the same role/profile/project.
//! - `project`: indexed source brief and skill catalog. Semi-stable; a source
//!   or skill edit invalidates this section without moving `core`.
//! - `volatile`: run context, objective, and the retrieved memory brief. It is
//!   always last and never receives an explicit cache breakpoint.
//!
//! This module contains no provider I/O. It owns hashes, breakpoint plans,
//! miss-reason attribution, savings math, and compaction cache generations so
//! the agent loop and protocol projections can share one deterministic model.

use serde_json::Value;
use vp_protocol::types::{CachePolicy, PromptSectionRef, SessionCacheCall};
use vp_providers::{
    CacheBreakpointPlan, CacheTtl, ChatMessage, PromptCacheConfig, SystemSection,
    plan_cache_breakpoints,
};

use crate::instruction::{
    InstructionSectionKey, MaterializedInstructions, PROMPT_LAYOUT_V2, PROMPT_LAYOUT_V3,
    normalize_prompt_layout,
};
use crate::sha256_hex;

pub const PROMPT_SECTION_VERSION: u32 = 2;
/// Prompt identity version emitted by the v3 instruction layout. It is
/// deliberately distinct from the wire `context.prompt_prefix.version` field
/// for backwards compatibility with older clients.
pub const PROMPT_SECTION_VERSION_V3: u32 = 3;

pub const CORE_SECTION: &str = "core";
pub const PROJECT_SECTION: &str = "project";
pub const VOLATILE_SECTION: &str = "volatile";
pub const TAIL_MEMORY_SECTION: &str = "tail/memory";

/// One ordered head/tail section with its exact text. The key is the stable
/// instruction key (`core/charter`, `run/task`, ...); the text is never copied
/// into telemetry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptHeading {
    pub key: String,
    pub text: String,
    pub ordinal: u32,
    pub tail: bool,
}

impl PromptHeading {
    pub fn new(key: impl Into<String>, text: impl Into<String>, ordinal: u32, tail: bool) -> Self {
        Self {
            key: key.into(),
            text: text.into(),
            ordinal,
            tail,
        }
    }
}

/// Deterministic system-prompt sections. `render()` is the backward-compatible
/// combined `system` string; adapters receive the exact boundaries separately.
///
/// Layout v2 populates only `core`, `project`, and `volatile`. Layout v3
/// populates `head`/`tail` in deterministic ordinal order, while `core` and
/// `project` remain joined compatibility views for older telemetry and tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptSections {
    pub core: String,
    pub project: String,
    pub volatile: String,
    /// `2` (legacy combined) or `3` (ordered instruction store).
    pub layout: u32,
    /// Ordered head sections sent before the conversation.
    pub head: Vec<PromptHeading>,
    /// Ordered tail sections sent after the conversation. `tail/memory` only.
    pub tail: Vec<PromptHeading>,
}

impl Default for PromptSections {
    fn default() -> Self {
        Self {
            core: String::new(),
            project: String::new(),
            volatile: String::new(),
            layout: PROMPT_LAYOUT_V2,
            head: Vec::new(),
            tail: Vec::new(),
        }
    }
}

impl PromptSections {
    pub fn new(
        core: impl Into<String>,
        project: impl Into<String>,
        volatile: impl Into<String>,
    ) -> Self {
        Self {
            core: core.into(),
            project: project.into(),
            volatile: volatile.into(),
            layout: PROMPT_LAYOUT_V2,
            head: Vec::new(),
            tail: Vec::new(),
        }
    }

    /// Build the v3 view from a materialized instruction scope. The
    /// compatibility `core`/`project` fields are joined views; they are never
    /// sent ahead of the ordered head sections.
    pub fn from_materialized(materialized: &MaterializedInstructions) -> Self {
        let head = materialized
            .head
            .iter()
            .map(|section| {
                PromptHeading::new(
                    section.key.key(),
                    section.text.clone(),
                    section.ordinal,
                    false,
                )
            })
            .collect::<Vec<_>>();
        let tail = materialized
            .tail
            .iter()
            .map(|section| {
                PromptHeading::new(
                    section.key.key(),
                    section.text.clone(),
                    section.ordinal,
                    true,
                )
            })
            .collect::<Vec<_>>();
        let core = join_headings(
            head.iter()
                .filter(|heading| heading.key.starts_with("core/")),
        );
        let project = join_headings(
            head.iter()
                .filter(|heading| heading.key.starts_with("project/")),
        );
        Self {
            core,
            project,
            volatile: String::new(),
            layout: PROMPT_LAYOUT_V3,
            head,
            tail,
        }
    }

    pub fn is_v3(&self) -> bool {
        normalize_prompt_layout(self.layout) == PROMPT_LAYOUT_V3
    }

    pub fn has_core(&self) -> bool {
        !self.core.is_empty()
    }

    pub fn has_project(&self) -> bool {
        !self.project.is_empty()
    }

    pub fn has_volatile(&self) -> bool {
        !self.volatile.is_empty()
    }

    pub fn has_tail(&self) -> bool {
        self.tail.iter().any(|heading| !heading.text.is_empty())
    }

    /// Combined compatibility string. In v2 the stable sections always come
    /// first and the volatile section is never moved ahead of them. In v3 the
    /// head sections are joined in ordinal order; tail memory is deliberately
    /// excluded.
    pub fn render(&self) -> String {
        if self.is_v3() {
            return join_headings(self.head.iter());
        }
        let mut rendered = String::new();
        if self.has_core() {
            rendered.push_str(&self.core);
        }
        if self.has_project() {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(&self.project);
        }
        if self.has_volatile() {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str(&self.volatile);
        }
        rendered
    }

    /// The stable prefix marker retained for older adapters and for the
    /// Anthropic legacy fallback. In v2 it is `core + project`, never volatile.
    /// In v3 it is every ordered head section, including session/run context,
    /// because those are stable for the scope and precede the conversation.
    pub fn stable_prefix(&self) -> String {
        if self.is_v3() {
            return join_headings(self.head.iter());
        }
        let mut stable = String::new();
        if self.has_core() {
            stable.push_str(&self.core);
        }
        if self.has_project() {
            if !stable.is_empty() {
                stable.push_str("\n\n");
            }
            stable.push_str(&self.project);
        }
        stable
    }

    /// Head sections for the provider. V2 keeps its three historical section
    /// kinds. V3 maps exact keys to breakpoint-safe provider kinds: only the
    /// last core section and the last project section carry the explicit
    /// `core`/`project` kind so the Anthropic four-breakpoint cap is preserved.
    pub fn provider_sections(&self) -> Vec<SystemSection> {
        if self.is_v3() {
            let last_core = self
                .head
                .iter()
                .rposition(|heading| heading.key.starts_with("core/"));
            let last_project = self
                .head
                .iter()
                .rposition(|heading| heading.key.starts_with("project/"));
            return self
                .head
                .iter()
                .enumerate()
                .filter(|(_, heading)| !heading.text.is_empty())
                .map(|(index, heading)| {
                    let kind = if Some(index) == last_project {
                        PROJECT_SECTION
                    } else if heading.key.starts_with("project/") {
                        "project-stable"
                    } else if Some(index) == last_core {
                        CORE_SECTION
                    } else if heading.key.starts_with("core/") {
                        "core-stable"
                    } else if heading.key == InstructionSectionKey::SessionContext.key() {
                        "session"
                    } else if heading.key == InstructionSectionKey::RunTask.key() {
                        "task"
                    } else {
                        "instruction"
                    };
                    SystemSection::new(kind, heading.text.clone())
                })
                .collect();
        }
        let mut sections = Vec::with_capacity(3);
        if self.has_core() {
            sections.push(SystemSection::new(CORE_SECTION, self.core.clone()));
        }
        if self.has_project() {
            sections.push(SystemSection::new(PROJECT_SECTION, self.project.clone()));
        }
        if self.has_volatile() {
            sections.push(SystemSection::new(VOLATILE_SECTION, self.volatile.clone()));
        }
        sections
    }

    /// Tail sections. V3 returns memory only; the provider adapter decides
    /// whether it can be a trailing system message or must be a `[memory]`
    /// user message.
    pub fn tail_sections(&self) -> Vec<SystemSection> {
        self.tail
            .iter()
            .filter(|heading| !heading.text.is_empty())
            .map(|heading| SystemSection::new(heading.key.clone(), heading.text.clone()))
            .collect()
    }

    pub fn section_chars(&self) -> std::collections::BTreeMap<String, u64> {
        if self.is_v3() {
            return self
                .head
                .iter()
                .chain(self.tail.iter())
                .map(|heading| (heading.key.clone(), heading.text.chars().count() as u64))
                .collect();
        }
        let mut chars = std::collections::BTreeMap::new();
        if !self.core.is_empty() {
            chars.insert(CORE_SECTION.to_string(), self.core.chars().count() as u64);
        }
        if !self.project.is_empty() {
            chars.insert(
                PROJECT_SECTION.to_string(),
                self.project.chars().count() as u64,
            );
        }
        if !self.volatile.is_empty() {
            chars.insert(
                VOLATILE_SECTION.to_string(),
                self.volatile.chars().count() as u64,
            );
        }
        chars
    }

    pub fn hashes(&self, model: &str, tools: &[vp_providers::ToolSchema]) -> PromptSectionHashes {
        PromptSectionHashes::compute(self, model, tools)
    }

    /// Test-only exact bytes for one heading, used to assert byte identity and
    /// tail isolation without exposing prompt text through telemetry.
    #[cfg(test)]
    pub fn exact_text(&self, key: &str) -> Option<&str> {
        self.head
            .iter()
            .chain(self.tail.iter())
            .find(|heading| heading.key == key)
            .map(|heading| heading.text.as_str())
    }
}

fn join_headings<'a>(headings: impl Iterator<Item = &'a PromptHeading>) -> String {
    let mut rendered = String::new();
    for heading in headings {
        if heading.text.is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(&heading.text);
    }
    rendered
}

/// Component hashes recorded on every prompt-prefix and cache event. They are
/// SHA-256 hex strings; no prompt text is persisted in cache telemetry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptSectionHashes {
    pub core_hash: String,
    pub project_hash: String,
    pub tools_hash: String,
    pub volatile_hash: String,
    pub stable_system_hash: String,
    /// Hash of `(version, model, tools, core, project)` in v2, or the ordered
    /// `(layout, model, tools, key/hash)` identity in v3. Volatile content and
    /// tail memory are intentionally excluded so memory changes cannot
    /// invalidate the shared head prefix.
    pub prefix_hash: String,
    /// `2` or `3`; the persisted layout that produced these hashes.
    pub prompt_layout: u32,
    /// Hash of the exact ordered head text. In v2 this equals
    /// `stable_system_hash`; in v3 it includes session/run context.
    pub stable_prefix_hash: String,
    /// Ordered head section references (key/hash/ordinal/tail=false).
    pub head_sections: Vec<PromptSectionRef>,
    /// Ordered tail section references (key/hash/ordinal/tail=true).
    pub tail_sections: Vec<PromptSectionRef>,
    pub section_chars: std::collections::BTreeMap<String, u64>,
}

impl PromptSectionHashes {
    pub fn compute(
        sections: &PromptSections,
        model: &str,
        tools: &[vp_providers::ToolSchema],
    ) -> Self {
        // Canonical schema serialization is owned by `tool_bundle`: tools are
        // name-sorted, parameter objects are recursively key-sorted, and every
        // selected schema is deduplicated. Session data is never included, so
        // the same bundle produces the same bytes in another session.
        let tools_hash = crate::tool_bundle::canonical_tools_hash(tools);
        let core_hash = sha256_hex(sections.core.as_bytes());
        let project_hash = sha256_hex(sections.project.as_bytes());
        let stable_prefix = sections.stable_prefix();
        let stable_system_hash = sha256_hex(stable_prefix.as_bytes());
        let stable_prefix_hash = stable_system_hash.clone();
        if sections.is_v3() {
            let head_sections = sections
                .head
                .iter()
                .filter(|heading| !heading.text.is_empty())
                .map(|heading| {
                    PromptSectionRef::new(
                        heading.key.clone(),
                        sha256_hex(heading.text.as_bytes()),
                        heading.ordinal,
                        false,
                    )
                })
                .collect::<Vec<_>>();
            let tail_sections = sections
                .tail
                .iter()
                .filter(|heading| !heading.text.is_empty())
                .map(|heading| {
                    PromptSectionRef::new(
                        heading.key.clone(),
                        sha256_hex(heading.text.as_bytes()),
                        heading.ordinal,
                        true,
                    )
                })
                .collect::<Vec<_>>();
            let volatile_hash = v3_volatile_hash(sections);
            let prefix_hash = prefix_hash_v3(model, &tools_hash, &head_sections);
            return Self {
                core_hash,
                project_hash,
                tools_hash,
                volatile_hash,
                stable_system_hash,
                prefix_hash,
                prompt_layout: PROMPT_LAYOUT_V3,
                stable_prefix_hash,
                head_sections,
                tail_sections,
                section_chars: sections.section_chars(),
            };
        }
        let volatile_hash = sha256_hex(sections.volatile.as_bytes());
        let prefix_hash = prefix_hash_from_parts(model, &tools_hash, &core_hash, &project_hash);
        let mut head_sections = Vec::new();
        if sections.has_core() {
            head_sections.push(PromptSectionRef::new(
                CORE_SECTION,
                core_hash.clone(),
                10,
                false,
            ));
        }
        if sections.has_project() {
            head_sections.push(PromptSectionRef::new(
                PROJECT_SECTION,
                project_hash.clone(),
                20,
                false,
            ));
        }
        Self {
            core_hash,
            project_hash,
            tools_hash,
            volatile_hash,
            stable_system_hash,
            prefix_hash,
            prompt_layout: PROMPT_LAYOUT_V2,
            stable_prefix_hash,
            head_sections,
            tail_sections: Vec::new(),
            section_chars: sections.section_chars(),
        }
    }

    pub fn payload(&self) -> Value {
        serde_json::json!({
            "core_hash": self.core_hash,
            "project_hash": self.project_hash,
            "tools_hash": self.tools_hash,
            "volatile_hash": self.volatile_hash,
            "stable_system_hash": self.stable_system_hash,
            "prefix_hash": self.prefix_hash,
            "prompt_layout": self.prompt_layout,
            "stable_prefix_hash": self.stable_prefix_hash,
            "head_sections": self.head_sections,
            "tail_sections": self.tail_sections,
            "section_chars": self.section_chars,
        })
    }

    /// Fingerprint used by miss attribution. `stable_system_hash` is omitted:
    /// it is a function of core+project and would be redundant.
    pub fn fingerprint(&self) -> CacheCallFingerprint {
        CacheCallFingerprint {
            core_hash: self.core_hash.clone(),
            project_hash: self.project_hash.clone(),
            tools_hash: self.tools_hash.clone(),
            volatile_hash: self.volatile_hash.clone(),
            prefix_hash: self.prefix_hash.clone(),
            cache_epoch: String::new(),
            model: String::new(),
        }
    }

    /// Fingerprint including the finalized L4 epoch. Callers that have an
    /// epoch must use this variant so same-epoch provider misses are not
    /// confused with local prefix changes.
    pub fn fingerprint_with_epoch(&self, cache_epoch: &str) -> CacheCallFingerprint {
        let mut fingerprint = self.fingerprint();
        fingerprint.cache_epoch = cache_epoch.to_string();
        fingerprint
    }

    /// Fingerprint including the finalized epoch and provider model.
    pub fn fingerprint_with_epoch_and_model(
        &self,
        cache_epoch: &str,
        model: &str,
    ) -> CacheCallFingerprint {
        let mut fingerprint = self.fingerprint_with_epoch(cache_epoch);
        fingerprint.model = model.to_string();
        fingerprint
    }
}

/// Hash of the reusable stable prefix. Version and model are included so a
/// model switch or an intentional prompt-layout revision creates a new
/// generation instead of silently reusing an incompatible prefix.
pub fn prefix_hash_from_parts(
    model: &str,
    tools_hash: &str,
    core_hash: &str,
    project_hash: &str,
) -> String {
    sha256_hex(
        format!("{PROMPT_SECTION_VERSION}\n{model}\n{tools_hash}\n{core_hash}\n{project_hash}")
            .as_bytes(),
    )
}

/// Ordered v3 prefix identity. The key name and ordinal travel with each hash
/// so a section reorder or key rename cannot accidentally reuse a provider
/// cache entry that represents different bytes.
pub fn prefix_hash_v3(model: &str, tools_hash: &str, head_sections: &[PromptSectionRef]) -> String {
    let mut material = format!("{PROMPT_SECTION_VERSION_V3}\n{model}\n{tools_hash}\n");
    for section in head_sections {
        material.push_str(&section.key);
        material.push('\u{1f}');
        material.push_str(&section.ordinal.to_string());
        material.push('\u{1f}');
        material.push_str(&section.hash);
        material.push('\u{1e}');
    }
    sha256_hex(material.as_bytes())
}

/// Volatile identity for v3: session/run head content plus tail memory. It
/// changes when a task or memory update changes, without changing the head
/// prefix hash.
fn v3_volatile_hash(sections: &PromptSections) -> String {
    let mut material = String::new();
    for heading in sections
        .head
        .iter()
        .filter(|heading| matches!(heading.key.as_str(), "session/context" | "run/task"))
    {
        material.push_str(&heading.key);
        material.push('\u{1f}');
        material.push_str(&sha256_hex(heading.text.as_bytes()));
        material.push('\u{1f}');
    }
    for heading in &sections.tail {
        material.push_str(&heading.key);
        material.push('\u{1f}');
        material.push_str(&sha256_hex(heading.text.as_bytes()));
        material.push('\u{1f}');
    }
    sha256_hex(material.as_bytes())
}

/// Map a project policy and role to the provider request policy.
pub fn prompt_cache_config(policy: &CachePolicy, role: &str) -> PromptCacheConfig {
    PromptCacheConfig::new(
        policy.enabled,
        CacheTtl::parse(policy.ttl_for(role)).unwrap_or_default(),
        policy.automatic_caching,
    )
}

/// Compute the actual provider plan for one request. The runtime records this
/// before the provider call; adapter tests assert the same wire shape.
pub fn request_cache_plan(
    provider_name: &str,
    sections: &PromptSections,
    tool_count: usize,
    message_count: usize,
    policy: &CachePolicy,
    role: &str,
) -> CacheBreakpointPlan {
    let config = prompt_cache_config(policy, role);
    plan_cache_breakpoints(
        provider_name,
        &sections.provider_sections(),
        tool_count,
        message_count,
        config,
    )
}

/// Cache-call identity for miss attribution. This is deliberately narrower
/// than the full prompt: only components that can invalidate the reusable
/// prefix take part.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CacheCallFingerprint {
    pub core_hash: String,
    pub project_hash: String,
    pub tools_hash: String,
    pub volatile_hash: String,
    pub prefix_hash: String,
    /// Finalized L4 epoch. Legacy events leave this empty; attribution then
    /// falls back to the prefix hash and never invents a cross-session reuse.
    pub cache_epoch: String,
    /// Provider-facing model. A model switch changes the epoch even when the
    /// text and tool schemas are byte-identical, so attribution names it
    /// explicitly instead of collapsing it into `unknown`.
    pub model: String,
}

impl CacheCallFingerprint {
    pub fn from_event_payload(payload: &Value) -> Option<Self> {
        let get = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
        let prefix_hash = get("prompt_prefix_hash").or_else(|| get("prefix_hash"))?;
        Some(Self {
            core_hash: get("core_hash").unwrap_or_default(),
            project_hash: get("project_hash").unwrap_or_default(),
            tools_hash: get("tools_hash").unwrap_or_default(),
            volatile_hash: get("volatile_hash").unwrap_or_default(),
            prefix_hash,
            cache_epoch: get("cache_epoch").unwrap_or_default(),
            model: get("model").unwrap_or_default(),
        })
    }
}

/// Derived miss attribution. A reason is recorded only when the provider
/// reported zero cache reads for the call; `None` means the call hit the
/// prefix cache or no attribution was needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheMissReason {
    FirstCall,
    PrefixChanged,
    ModelChanged,
    ToolsChanged,
    ProjectChanged,
    /// Same epoch, same tools/project, but the provider reported a low or zero
    /// cache hit. This is a provider/TTL observation, not local prefix churn.
    ProviderCacheMiss,
    /// Legacy attribution for events that predate component hashes.
    TtlOrProviderMiss,
    CompactionReset,
    Unknown,
}

impl CacheMissReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FirstCall => "first_call",
            Self::PrefixChanged => "prefix_changed",
            Self::ModelChanged => "model_changed",
            Self::ToolsChanged => "tools_changed",
            Self::ProjectChanged => "project_changed",
            Self::ProviderCacheMiss => "provider_cache_miss",
            Self::TtlOrProviderMiss => "ttl_or_provider_miss",
            Self::CompactionReset => "compaction_reset",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "first_call" => Self::FirstCall,
            "prefix_changed" => Self::PrefixChanged,
            "model_changed" => Self::ModelChanged,
            "tools_changed" => Self::ToolsChanged,
            "project_changed" => Self::ProjectChanged,
            "provider_cache_miss" => Self::ProviderCacheMiss,
            "ttl_or_provider_miss" => Self::TtlOrProviderMiss,
            "compaction_reset" => Self::CompactionReset,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    /// True when the reason points at a local identity change rather than at
    /// provider eviction or a cold start.
    pub fn is_prefix_change(self) -> bool {
        matches!(
            self,
            Self::PrefixChanged | Self::ModelChanged | Self::ToolsChanged | Self::ProjectChanged
        )
    }
}

/// In-run miss tracker. It is seeded from the latest persisted cache call so a
/// resumed run can still attribute a tool/project change correctly, and it
/// marks the next observation after a successful compaction.
#[derive(Debug, Clone, Default)]
pub struct CacheCallTracker {
    last: Option<CacheCallFingerprint>,
    compaction_pending: bool,
}

impl CacheCallTracker {
    pub fn new(last: Option<CacheCallFingerprint>) -> Self {
        Self {
            last,
            compaction_pending: false,
        }
    }

    pub fn from_event(payload: Option<&Value>) -> Self {
        Self::new(payload.and_then(CacheCallFingerprint::from_event_payload))
    }

    pub fn last(&self) -> Option<&CacheCallFingerprint> {
        self.last.as_ref()
    }

    pub fn mark_compaction(&mut self) {
        self.compaction_pending = true;
    }

    pub fn compaction_pending(&self) -> bool {
        self.compaction_pending
    }

    /// Observe one call with only cache-read tokens. Retained for callers and
    /// tests that do not have the full provider input count; it delegates to
    /// [`Self::observe_call`] with an input equal to the cache reads so a hit
    /// stays a hit and a zero-read call remains attributable.
    pub fn observe(
        &mut self,
        current: &CacheCallFingerprint,
        cache_read_tokens: u64,
    ) -> Option<CacheMissReason> {
        self.observe_call(
            current,
            CacheCallObservation {
                // A zero-read call must still be classified. With no input
                // count, one token is the smallest honest denominator.
                input_tokens: cache_read_tokens.max(1),
                cache_read_tokens,
                first_session_call: false,
                prior_sessions: 0,
            },
        )
    }

    /// Observe one provider call and return a miss reason when the reported
    /// hit is low.
    ///
    /// Attribution order:
    ///
    /// 1. a changed tool hash → `tools_changed`;
    /// 2. a changed project hash → `project_changed`;
    /// 3. a changed core hash or full prefix hash (with the same epoch) →
    ///    `prefix_changed`;
    /// 4. the first call after compaction → `compaction_reset`;
    /// 5. same epoch with a low hit → `provider_cache_miss`;
    /// 6. no previous call and no known session for the epoch → `first_call`;
    /// 7. legacy/no component hashes → `ttl_or_provider_miss`.
    ///
    /// A hit clears the compaction marker because the following call's prefix
    /// is no longer the pre-compaction generation.
    pub fn observe_call(
        &mut self,
        current: &CacheCallFingerprint,
        observation: CacheCallObservation,
    ) -> Option<CacheMissReason> {
        if !crate::cache_epoch::is_low_cache_hit(
            observation.input_tokens,
            observation.cache_read_tokens,
        ) {
            self.last = Some(current.clone());
            self.compaction_pending = false;
            return None;
        }
        let reason = if let Some(last) = self.last.as_ref() {
            if !last.model.is_empty() && !current.model.is_empty() && last.model != current.model {
                Some(CacheMissReason::ModelChanged)
            } else if last.tools_hash != current.tools_hash {
                Some(CacheMissReason::ToolsChanged)
            } else if last.project_hash != current.project_hash {
                Some(CacheMissReason::ProjectChanged)
            } else if last.core_hash != current.core_hash
                || (last.prefix_hash != current.prefix_hash
                    && last.cache_epoch == current.cache_epoch)
            {
                Some(CacheMissReason::PrefixChanged)
            } else if self.compaction_pending {
                Some(CacheMissReason::CompactionReset)
            } else if !current.cache_epoch.is_empty() && last.cache_epoch == current.cache_epoch {
                Some(CacheMissReason::ProviderCacheMiss)
            } else if current.cache_epoch.is_empty() {
                Some(CacheMissReason::TtlOrProviderMiss)
            } else {
                Some(CacheMissReason::Unknown)
            }
        } else if self.compaction_pending {
            Some(CacheMissReason::CompactionReset)
        } else if observation.prior_sessions > 0 && !current.cache_epoch.is_empty() {
            // The epoch existed in this project, so a cold provider cache is
            // an eviction observation rather than a brand-new session miss.
            Some(CacheMissReason::ProviderCacheMiss)
        } else if observation.first_session_call || current.cache_epoch.is_empty() {
            Some(CacheMissReason::FirstCall)
        } else {
            Some(CacheMissReason::ProviderCacheMiss)
        };
        self.last = Some(current.clone());
        // The marker describes the next observation only. Whether that call
        // hit or missed, a later call compares against the new fingerprint and
        // must not keep reporting the same compaction reset.
        self.compaction_pending = false;
        reason
    }
}

/// Provider-reported shape for one call. `first_session_call` and
/// `prior_sessions` come from the persisted epoch projection, not from an
/// in-memory guess.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CacheCallObservation {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub first_session_call: bool,
    pub prior_sessions: u64,
}

/// Estimated no-cache cost minus billed cost for one call. `input` already
/// includes cache reads and writes. Negative values are valid when cache
/// writes cost more than the reads they save; the runtime reports them honestly
/// instead of clamping the per-call number to look good.
#[allow(clippy::too_many_arguments)]
pub fn estimated_cache_savings_usd(
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    input_price: f64,
    output_price: f64,
    cache_read_price: f64,
    cache_write_price: f64,
) -> f64 {
    let cache_read = cache_read.min(input);
    let cache_write = cache_write.min(input.saturating_sub(cache_read));
    let uncached = input.saturating_sub(cache_read).saturating_sub(cache_write);
    let no_cache = per_million(input, input_price) + per_million(output, output_price);
    let billed = per_million(uncached, input_price)
        + per_million(cache_read, cache_read_price)
        + per_million(cache_write, cache_write_price)
        + per_million(output, output_price);
    let savings = no_cache - billed;
    if savings.is_finite() { savings } else { 0.0 }
}

fn per_million(tokens: u64, price: f64) -> f64 {
    tokens as f64 * price / 1_000_000.0
}

/// Hash of the active conversation generation at one instant. Compaction
/// changes this even when the stable system/tool prefix is unchanged.
pub fn conversation_generation(prefix_hash: &str, messages: &[ChatMessage]) -> String {
    let mut material = String::with_capacity(prefix_hash.len() + messages.len() * 32);
    material.push_str(prefix_hash);
    material.push('\u{1f}');
    for message in messages {
        material.push_str(&message.role);
        material.push('\u{1f}');
        material.push_str(&message.content.len().to_string());
        material.push('\u{1f}');
        material.push_str(&message.content);
        material.push('\u{1f}');
        if let Some(calls) = &message.tool_calls {
            material.push_str(&serde_json::to_string(calls).unwrap_or_default());
        }
        material.push('\u{1f}');
        if let Some(call_id) = &message.tool_call_id {
            material.push_str(call_id);
        }
        material.push('\u{1e}');
    }
    sha256_hex(material.as_bytes())
}

/// Round a USD estimate for persisted telemetry without pretending to have
/// invoice precision.
pub fn round_usd(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }
    (value * 1_000_000.0).round() / 1_000_000.0
}

/// Build the bounded protocol record for one `context.cache` event payload.
pub fn cache_call_record(payload: &Value, created_at: &str) -> SessionCacheCall {
    let u64 = |key: &str| payload.get(key).and_then(Value::as_u64).unwrap_or(0);
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
    SessionCacheCall {
        at: created_at.to_string(),
        agent_name: text("agent_name"),
        role: text("agent_role"),
        model: text("model"),
        input_tokens: u64("input_tokens"),
        output_tokens: u64("output_tokens"),
        cache_read_tokens: u64("cache_read_tokens"),
        cache_write_tokens: u64("cache_write_tokens"),
        cache_hit_ratio: payload
            .get("cache_hit_ratio")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        cache_breakpoints: payload
            .get("cache_breakpoints")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        cache_ttl: text("cache_ttl"),
        first_token_ms: payload.get("first_token_ms").and_then(Value::as_u64),
        core_hash: text("core_hash"),
        project_hash: text("project_hash"),
        tools_hash: text("tools_hash"),
        tool_bundle: text("tool_bundle"),
        tool_bundle_hash: text("tool_bundle_hash"),
        volatile_hash: text("volatile_hash"),
        prompt_prefix_hash: text("prompt_prefix_hash"),
        canonical_version: payload
            .get("canonical_version")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        previous_message_prefix_hash: text("previous_message_prefix_hash")
            .filter(|hash| !hash.is_empty()),
        message_prefix_hash: text("message_prefix_hash"),
        append_only: payload
            .get("append_only")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        message_count: u64("message_count"),
        head_prefix_hash: text("head_prefix_hash"),
        tail_prefix_hash: text("tail_prefix_hash"),
        request_prefix_hash: text("request_prefix_hash"),
        tail_memory_mode: text("tail_memory_mode"),
        provider_tail_system: payload.get("provider_tail_system").and_then(Value::as_bool),
        tail_system_capability: text("tail_system_capability"),
        cache_break_reason: text("cache_break_reason"),
        message_sequence_issue_count: payload
            .get("message_sequence_issue_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                payload
                    .get("message_sequence_issues")
                    .and_then(Value::as_array)
                    .map_or(0, |issues| issues.len() as u64)
            }),
        message_sequence_issues: payload
            .get("message_sequence_issues")
            .and_then(Value::as_array)
            .map(|issues| {
                issues
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        cache_epoch: text("cache_epoch"),
        cache_epoch_reused_from_sessions: u64("cache_epoch_reused_from_sessions"),
        cross_session_cache_read_tokens: u64("cross_session_cache_read_tokens"),
        cross_session_first_call: payload
            .get("cross_session_first_call")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        estimated_savings_usd: payload
            .get("estimated_savings_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        miss_reason: text("miss_reason"),
        warm: payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Decode one bounded `context.cache_break` event for the protocol projection.
pub fn cache_break_record(
    payload: &Value,
    created_at: &str,
) -> vp_protocol::types::SessionCacheBreak {
    use vp_protocol::types::SessionCacheBreak;
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(str::to_string);
    let u64 = |key: &str| payload.get(key).and_then(Value::as_u64).unwrap_or(0);
    SessionCacheBreak {
        at: created_at.to_string(),
        reason: text("reason").unwrap_or_else(|| "unknown".to_string()),
        agent_name: text("agent_name"),
        role: text("agent_role").or_else(|| text("role")),
        model: text("model"),
        provider: text("provider"),
        first_differing_index: payload.get("first_differing_index").and_then(Value::as_u64),
        first_differing_role: text("first_differing_role"),
        first_differing_tool_call_id: text("first_differing_tool_call_id"),
        previous_message_count: u64("previous_message_count"),
        message_count: u64("message_count"),
        previous_message_prefix_hash: text("previous_message_prefix_hash")
            .filter(|hash| !hash.is_empty()),
        message_prefix_hash: text("message_prefix_hash"),
        warm: payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// Aggregates a bounded window of cache and compaction events for the
/// `session.cache` projection. All values are derived from persisted telemetry;
/// missing or legacy payloads contribute zero rather than a fabricated fact.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheTelemetryAggregate {
    pub calls: u64,
    pub warm_calls: u64,
    pub miss_reason_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub ttl_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub breakpoint_counts: Vec<vp_protocol::types::CacheCountBucket>,
    pub savings_by_role: Vec<vp_protocol::types::CacheSavingsBucket>,
    pub first_token_p50_ms: Option<u64>,
    pub first_token_p95_ms: Option<u64>,
    pub compaction_resets: u64,
    pub compaction_invalidation_tokens: u64,
    pub compaction_invalidation_estimated_usd: f64,
    /// Epochs observed in the bounded cache window, sorted by epoch hash.
    pub cache_epoch_counts: Vec<vp_protocol::types::CacheCountBucket>,
    /// Provider-reported first-call reuse attributed to another session.
    pub cross_session_hit_tokens: u64,
    pub cross_session_first_calls: u64,
    /// Same-epoch low-hit calls attributed to provider eviction/TTL.
    pub provider_cache_misses: u64,
    /// Calls attributed to a local model/tools/project/prefix change.
    pub prefix_changes: u64,
    /// Always-computed lost cache-read estimate from reset events.
    pub compaction_lost_cache_usd: f64,
}

/// Nearest-rank percentile over a small sorted sample. Returns `None` for an
/// empty sample; never invents a latency for missing telemetry.
fn nearest_rank(sorted: &[u64], percentile: u64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percentile as f64 / 100.0 * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    Some(sorted[index])
}

/// Aggregate cache calls and compaction resets. Buckets are sorted by key so
/// repeated calls produce byte-identical protocol output.
pub fn aggregate_cache_events(
    cache_events: &[vp_protocol::types::Event],
    compaction_events: &[vp_protocol::types::Event],
) -> CacheTelemetryAggregate {
    use std::collections::BTreeMap;

    let mut aggregate = CacheTelemetryAggregate::default();
    let mut misses: BTreeMap<String, u64> = BTreeMap::new();
    let mut ttls: BTreeMap<String, u64> = BTreeMap::new();
    let mut breakpoints: BTreeMap<String, u64> = BTreeMap::new();
    let mut savings: BTreeMap<String, (u64, f64)> = BTreeMap::new();
    let mut epochs: BTreeMap<String, u64> = BTreeMap::new();
    let mut first_tokens = Vec::new();
    for event in cache_events {
        let payload = &event.payload;
        let warm = payload
            .get("warm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if warm {
            aggregate.warm_calls = aggregate.warm_calls.saturating_add(1);
        } else {
            aggregate.calls = aggregate.calls.saturating_add(1);
        }
        if let Some(epoch) = payload
            .get("cache_epoch")
            .and_then(Value::as_str)
            .filter(|value| crate::cache_epoch::is_cache_epoch(value))
        {
            *epochs.entry(epoch.to_string()).or_default() += 1;
        }
        if !warm {
            if let Some(reason) = payload.get("miss_reason").and_then(Value::as_str)
                && let Some(reason) = CacheMissReason::parse(reason)
            {
                if reason == CacheMissReason::ProviderCacheMiss {
                    aggregate.provider_cache_misses =
                        aggregate.provider_cache_misses.saturating_add(1);
                }
                if reason.is_prefix_change() {
                    aggregate.prefix_changes = aggregate.prefix_changes.saturating_add(1);
                }
            }
            if payload
                .get("cross_session_first_call")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                aggregate.cross_session_first_calls =
                    aggregate.cross_session_first_calls.saturating_add(1);
            }
            aggregate.cross_session_hit_tokens = aggregate.cross_session_hit_tokens.saturating_add(
                payload
                    .get("cross_session_cache_read_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            );
        }
        if let Some(reason) = payload.get("miss_reason").and_then(Value::as_str) {
            *misses.entry(reason.to_string()).or_default() += 1;
        }
        if let Some(ttl) = payload.get("cache_ttl").and_then(Value::as_str) {
            *ttls.entry(ttl.to_string()).or_default() += 1;
        }
        if let Some(count) = payload.get("cache_breakpoints").and_then(Value::as_u64) {
            *breakpoints.entry(count.to_string()).or_default() += 1;
        }
        let role = payload
            .get("agent_role")
            .and_then(Value::as_str)
            .unwrap_or("(unassigned)")
            .to_string();
        let value = payload
            .get("estimated_savings_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let entry = savings.entry(role).or_insert((0, 0.0));
        entry.0 = entry.0.saturating_add(1);
        entry.1 += value;
        if let Some(latency) = payload.get("first_token_ms").and_then(Value::as_u64)
            && latency > 0
        {
            first_tokens.push(latency);
        }
    }
    first_tokens.sort_unstable();
    aggregate.first_token_p50_ms = nearest_rank(&first_tokens, 50);
    aggregate.first_token_p95_ms = nearest_rank(&first_tokens, 95);
    aggregate.miss_reason_counts = misses
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.ttl_counts = ttls
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.breakpoint_counts = breakpoints
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.cache_epoch_counts = epochs
        .into_iter()
        .map(|(key, calls)| vp_protocol::types::CacheCountBucket { key, calls })
        .collect();
    aggregate.savings_by_role = savings
        .into_iter()
        .map(
            |(role, (calls, estimated_savings_usd))| vp_protocol::types::CacheSavingsBucket {
                role,
                calls,
                estimated_savings_usd: round_usd(estimated_savings_usd),
            },
        )
        .collect();
    for event in compaction_events {
        let reason = event
            .payload
            .get("cache_reset_reason")
            .or_else(|| event.payload.get("reset_reason"))
            .and_then(Value::as_str);
        if reason == Some("compaction") {
            aggregate.compaction_resets = aggregate.compaction_resets.saturating_add(1);
        }
        let invalidated = event
            .payload
            .get("cache_invalidation_tokens")
            .or_else(|| event.payload.get("invalidated_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        aggregate.compaction_invalidation_tokens = aggregate
            .compaction_invalidation_tokens
            .saturating_add(invalidated);
        let lost = event
            .payload
            .get("cache_invalidation_estimated_usd")
            .or_else(|| event.payload.get("estimated_lost_cache_read_usd"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        aggregate.compaction_invalidation_estimated_usd += lost;
        aggregate.compaction_lost_cache_usd += lost;
    }
    aggregate.compaction_invalidation_estimated_usd =
        round_usd(aggregate.compaction_invalidation_estimated_usd);
    aggregate.compaction_lost_cache_usd = round_usd(aggregate.compaction_lost_cache_usd);
    aggregate
}

#[cfg(test)]
mod tests {
    use super::*;
    use vp_providers::ToolSchema;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn sections() -> PromptSections {
        PromptSections::new("role charter", "sources and skills", "run context")
    }

    #[test]
    fn render_is_stable_first_and_skips_empty_sections() {
        assert_eq!(
            PromptSections::new("core", "", "volatile").render(),
            "core\n\nvolatile"
        );
        assert_eq!(
            PromptSections::new("", "project", "volatile").render(),
            "project\n\nvolatile"
        );
        assert_eq!(PromptSections::new("core", "", "").render(), "core");
        assert_eq!(PromptSections::default().render(), "");
    }

    #[test]
    fn provider_sections_preserve_core_project_volatile_order() {
        let provider = sections().provider_sections();
        assert_eq!(provider.len(), 3);
        assert_eq!(provider[0].kind, CORE_SECTION);
        assert_eq!(provider[1].kind, PROJECT_SECTION);
        assert_eq!(provider[2].kind, VOLATILE_SECTION);
        assert_eq!(provider[0].text, "role charter");
        assert_eq!(provider[1].text, "sources and skills");
        assert_eq!(provider[2].text, "run context");
    }

    #[test]
    fn memory_brief_never_changes_the_shared_prefix() {
        let tools = vec![tool("read_file")];
        let without_memory = sections().hashes("model", &tools);
        let mut with_memory = sections();
        with_memory.volatile = format!(
            "{}\nRun memory: use the API key from 1Password.",
            with_memory.volatile
        );
        let with_memory = with_memory.hashes("model", &tools);
        assert_eq!(without_memory.core_hash, with_memory.core_hash);
        assert_eq!(without_memory.project_hash, with_memory.project_hash);
        assert_eq!(without_memory.tools_hash, with_memory.tools_hash);
        assert_eq!(without_memory.prefix_hash, with_memory.prefix_hash);
        assert_ne!(without_memory.volatile_hash, with_memory.volatile_hash);
    }

    #[test]
    fn source_and_skill_changes_invalidate_only_the_project_component() {
        let tools = vec![tool("read_file")];
        let baseline = sections().hashes("model", &tools);
        let mut updated = sections();
        updated.project = "sources: new chunk; skills: rust-review".to_string();
        let updated = updated.hashes("model", &tools);
        assert_eq!(baseline.core_hash, updated.core_hash);
        assert_eq!(baseline.tools_hash, updated.tools_hash);
        assert_eq!(baseline.volatile_hash, updated.volatile_hash);
        assert_ne!(baseline.project_hash, updated.project_hash);
        assert_ne!(baseline.prefix_hash, updated.prefix_hash);
    }

    #[test]
    fn tool_changes_invalidate_tools_and_prefix_but_not_sections() {
        let first = sections().hashes("model", &[tool("read_file")]);
        let second = sections().hashes("model", &[tool("read_file"), tool("write_file")]);
        assert_eq!(first.core_hash, second.core_hash);
        assert_eq!(first.project_hash, second.project_hash);
        assert_ne!(first.tools_hash, second.tools_hash);
        assert_ne!(first.prefix_hash, second.prefix_hash);
    }

    #[test]
    fn tool_order_does_not_change_the_tools_hash() {
        let first = sections().hashes("model", &[tool("write_file"), tool("read_file")]);
        let second = sections().hashes("model", &[tool("read_file"), tool("write_file")]);
        assert_eq!(first.tools_hash, second.tools_hash);
        assert_eq!(first.prefix_hash, second.prefix_hash);
    }

    #[test]
    fn model_change_creates_a_new_prefix_generation() {
        let tools = vec![tool("read_file")];
        let first = sections().hashes("model-a", &tools);
        let second = sections().hashes("model-b", &tools);
        assert_ne!(first.prefix_hash, second.prefix_hash);
    }

    #[test]
    fn cache_policy_role_override_is_applied_without_inference() {
        let mut policy = CachePolicy::default();
        policy
            .role_ttl
            .insert("worker".to_string(), "1h".to_string());
        assert_eq!(
            prompt_cache_config(&policy, "leader").ttl,
            CacheTtl::FiveMinutes
        );
        assert_eq!(
            prompt_cache_config(&policy, "worker").ttl,
            CacheTtl::OneHour
        );
        assert!(prompt_cache_config(&policy, "leader").automatic);
    }

    #[test]
    fn request_plan_matches_adapter_breakpoint_limits() {
        let policy = CachePolicy::default();
        let plan = request_cache_plan("anthropic", &sections(), 4, 2, &policy, "leader");
        assert_eq!(plan.breakpoints, 4);
        let openai = request_cache_plan("openai-compatible", &sections(), 4, 2, &policy, "leader");
        assert_eq!(openai.breakpoints, 0);
        assert!(openai.automatic);
    }

    #[test]
    fn first_call_is_attributed_once() {
        let mut tracker = CacheCallTracker::default();
        let current = sections().hashes("model", &[]).fingerprint();
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::FirstCall)
        );
        // A second call with the same prefix and no reads is a TTL/provider
        // miss, not another first call.
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::TtlOrProviderMiss)
        );
    }

    #[test]
    fn hits_do_not_produce_a_miss_reason() {
        let mut tracker = CacheCallTracker::default();
        let current = sections().hashes("model", &[]).fingerprint();
        assert_eq!(tracker.observe(&current, 1_024), None);
    }

    #[test]
    fn tools_changed_takes_priority_over_project_changes() {
        let tools = vec![tool("read_file")];
        let first = sections().hashes("model", &tools);
        let mut tracker = CacheCallTracker::new(Some(first.fingerprint()));
        let mut updated = sections();
        updated.project = "new sources".to_string();
        let second = updated.hashes("model", &[tool("read_file"), tool("write_file")]);
        assert_eq!(
            tracker.observe(&second.fingerprint(), 0),
            Some(CacheMissReason::ToolsChanged)
        );
    }

    #[test]
    fn project_change_is_distinguished_from_prefix_change() {
        let first = sections().hashes("model", &[]);
        let mut tracker = CacheCallTracker::new(Some(first.fingerprint()));
        let mut updated = sections();
        updated.project = "new sources".to_string();
        assert_eq!(
            tracker.observe(&updated.hashes("model", &[]).fingerprint(), 0),
            Some(CacheMissReason::ProjectChanged)
        );
    }

    #[test]
    fn core_change_is_a_prefix_change() {
        let first = sections().hashes("model", &[]);
        let mut tracker = CacheCallTracker::new(Some(first.fingerprint()));
        let mut updated = sections();
        updated.core = "new charter".to_string();
        assert_eq!(
            tracker.observe(&updated.hashes("model", &[]).fingerprint(), 0),
            Some(CacheMissReason::PrefixChanged)
        );
    }

    #[test]
    fn compaction_reset_is_attributed_only_when_the_prefix_is_otherwise_stable() {
        let current = sections().hashes("model", &[]).fingerprint();
        let mut tracker = CacheCallTracker::new(Some(current.clone()));
        tracker.mark_compaction();
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::CompactionReset)
        );
        assert!(!tracker.compaction_pending());
    }

    #[test]
    fn a_hit_clears_the_compaction_marker() {
        let current = sections().hashes("model", &[]).fingerprint();
        let mut tracker = CacheCallTracker::new(Some(current.clone()));
        tracker.mark_compaction();
        assert_eq!(tracker.observe(&current, 512), None);
        assert!(!tracker.compaction_pending());
        assert_eq!(
            tracker.observe(&current, 0),
            Some(CacheMissReason::TtlOrProviderMiss)
        );
    }

    #[test]
    fn fingerprint_reads_legacy_and_new_event_keys() {
        let payload = serde_json::json!({
            "prefix_hash": "prefix",
            "core_hash": "core",
            "tools_hash": "tools",
        });
        let fingerprint = CacheCallFingerprint::from_event_payload(&payload).unwrap();
        assert_eq!(fingerprint.prefix_hash, "prefix");
        assert_eq!(fingerprint.core_hash, "core");
        assert_eq!(fingerprint.tools_hash, "tools");
        assert_eq!(fingerprint.project_hash, "");
        let new_payload = serde_json::json!({
            "prompt_prefix_hash": "new-prefix",
            "project_hash": "project",
        });
        assert_eq!(
            CacheCallFingerprint::from_event_payload(&new_payload)
                .unwrap()
                .prefix_hash,
            "new-prefix"
        );
        assert!(CacheCallFingerprint::from_event_payload(&serde_json::json!({})).is_none());
    }

    #[test]
    fn savings_math_prices_cache_reads_as_billed_not_free() {
        // 1M input at $3, 60% read at $0.3, 10% write at $3.75.
        let savings =
            estimated_cache_savings_usd(1_000_000, 0, 600_000, 100_000, 3.0, 0.0, 0.3, 3.75);
        // No-cache $3.00; billed 0.3M*$3 + 0.6M*$0.3 + 0.1M*$3.75 = $1.455.
        assert!((savings - 1.545).abs() < 1e-9, "{savings}");
    }

    #[test]
    fn savings_can_be_negative_when_cache_writes_cost_more_than_reads_save() {
        let savings = estimated_cache_savings_usd(100_000, 0, 0, 100_000, 3.0, 0.0, 0.3, 6.0);
        assert!(savings < 0.0, "{savings}");
        assert!((savings + 0.3).abs() < 1e-9, "{savings}");
    }

    #[test]
    fn savings_are_zero_when_the_provider_did_not_use_cache() {
        assert_eq!(
            estimated_cache_savings_usd(1_000, 500, 0, 0, 3.0, 15.0, 0.3, 3.75),
            0.0
        );
    }

    #[test]
    fn conversation_generation_changes_after_a_compaction_boundary() {
        let prefix = "stable-prefix";
        let before = vec![
            ChatMessage::user("one"),
            ChatMessage::assistant_tool_calls("", Vec::new()),
            ChatMessage::user("two"),
        ];
        let after = vec![ChatMessage::user("Earlier conversation summary")];
        assert_ne!(
            conversation_generation(prefix, &before),
            conversation_generation(prefix, &after)
        );
        assert_eq!(
            conversation_generation(prefix, &before),
            conversation_generation(prefix, &before)
        );
    }

    #[test]
    fn cache_call_record_tolerates_legacy_payloads() {
        let record = cache_call_record(&serde_json::json!({"input_tokens": 100}), "now");
        assert_eq!(record.input_tokens, 100);
        assert_eq!(record.cache_breakpoints, 0);
        assert!(!record.warm);
        assert_eq!(record.at, "now");
    }

    #[test]
    fn cache_call_record_reads_new_telemetry() {
        let payload = serde_json::json!({
            "agent_name": "Leader",
            "agent_role": "leader",
            "model": "claude",
            "input_tokens": 1000,
            "output_tokens": 50,
            "cache_read_tokens": 600,
            "cache_write_tokens": 100,
            "cache_hit_ratio": 0.6,
            "cache_breakpoints": 4,
            "cache_ttl": "1h",
            "first_token_ms": 321,
            "core_hash": "core",
            "project_hash": "project",
            "tools_hash": "tools",
            "volatile_hash": "volatile",
            "prompt_prefix_hash": "prefix",
            "estimated_savings_usd": 0.25,
            "miss_reason": "ttl_or_provider_miss",
            "warm": false,
            "canonical_version": 1,
            "previous_message_prefix_hash": "previous",
            "message_prefix_hash": "chain",
            "append_only": true,
            "message_count": 7,
            "head_prefix_hash": "head",
            "tail_prefix_hash": "tail",
            "request_prefix_hash": "request",
            "tail_memory_mode": "embedded",
            "provider_tail_system": true,
            "tail_system_capability": "supported",
            "cache_break_reason": "message_edited",
            "message_sequence_issue_count": 2,
            "message_sequence_issues": ["orphan_tool_result", "unanswered_tool_call"],
        });
        let record = cache_call_record(&payload, "now");
        assert_eq!(record.cache_breakpoints, 4);
        assert_eq!(record.cache_ttl.as_deref(), Some("1h"));
        assert_eq!(record.miss_reason.as_deref(), Some("ttl_or_provider_miss"));
        assert_eq!(record.estimated_savings_usd, 0.25);
        assert_eq!(record.first_token_ms, Some(321));
        assert_eq!(record.canonical_version, 1);
        assert_eq!(
            record.previous_message_prefix_hash.as_deref(),
            Some("previous")
        );
        assert_eq!(record.message_prefix_hash.as_deref(), Some("chain"));
        assert!(record.append_only);
        assert_eq!(record.message_count, 7);
        assert_eq!(record.tail_memory_mode.as_deref(), Some("embedded"));
        assert_eq!(record.provider_tail_system, Some(true));
        assert_eq!(record.cache_break_reason.as_deref(), Some("message_edited"));
        assert_eq!(record.message_sequence_issue_count, 2);
        assert_eq!(record.message_sequence_issues.len(), 2);
        assert_eq!(record.request_prefix_hash.as_deref(), Some("request"));
    }

    #[test]
    fn cache_break_record_tolerates_legacy_payloads() {
        let record = cache_break_record(&serde_json::json!({}), "now");
        assert_eq!(record.reason, "unknown");
        assert_eq!(record.message_count, 0);
        assert!(record.first_differing_index.is_none());
        let record = cache_break_record(
            &serde_json::json!({
                "reason": "tool_bundle_changed",
                "agent_name": "Leader",
                "agent_role": "leader",
                "provider": "deepseek",
                "model": "deepseek-flash",
                "previous_message_count": 9,
                "message_count": 10,
                "previous_message_prefix_hash": "previous",
                "message_prefix_hash": "chain",
                "warm": false
            }),
            "now",
        );
        assert_eq!(record.reason, "tool_bundle_changed");
        assert_eq!(record.role.as_deref(), Some("leader"));
        assert_eq!(record.provider.as_deref(), Some("deepseek"));
        assert_eq!(record.previous_message_count, 9);
        assert_eq!(record.message_count, 10);
        assert_eq!(
            record.previous_message_prefix_hash.as_deref(),
            Some("previous")
        );
    }

    fn event(kind: &str, payload: serde_json::Value) -> vp_protocol::types::Event {
        vp_protocol::types::Event {
            session_id: "ses-1".into(),
            seq: 1,
            kind: kind.into(),
            payload,
            created_at: "2026-11-11T00:00:00Z".into(),
        }
    }

    #[test]
    fn aggregate_orders_buckets_and_separates_warm_calls() {
        let events = vec![
            event(
                "context.cache",
                serde_json::json!({
                    "agent_role": "worker",
                    "cache_ttl": "1h",
                    "cache_breakpoints": 4,
                    "miss_reason": "tools_changed",
                    "estimated_savings_usd": 0.5,
                    "first_token_ms": 100,
                    "warm": false
                }),
            ),
            event(
                "context.cache",
                serde_json::json!({
                    "agent_role": "leader",
                    "cache_ttl": "5m",
                    "cache_breakpoints": 4,
                    "miss_reason": "first_call",
                    "estimated_savings_usd": -0.1,
                    "first_token_ms": 300,
                    "warm": false
                }),
            ),
            event(
                "context.cache",
                serde_json::json!({
                    "agent_role": "leader",
                    "cache_ttl": "5m",
                    "cache_breakpoints": 3,
                    "estimated_savings_usd": 0.0,
                    "first_token_ms": 200,
                    "warm": true
                }),
            ),
        ];
        let aggregate = aggregate_cache_events(&events, &[]);
        assert_eq!(aggregate.calls, 2);
        assert_eq!(aggregate.warm_calls, 1);
        assert_eq!(aggregate.first_token_p50_ms, Some(200));
        assert_eq!(aggregate.first_token_p95_ms, Some(300));
        assert_eq!(
            aggregate.miss_reason_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "first_call".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "tools_changed".into(),
                    calls: 1
                }
            ]
        );
        assert_eq!(
            aggregate.ttl_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "1h".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "5m".into(),
                    calls: 2
                }
            ]
        );
        assert_eq!(
            aggregate.breakpoint_counts,
            vec![
                vp_protocol::types::CacheCountBucket {
                    key: "3".into(),
                    calls: 1
                },
                vp_protocol::types::CacheCountBucket {
                    key: "4".into(),
                    calls: 2
                }
            ]
        );
        assert_eq!(aggregate.savings_by_role.len(), 2);
        assert_eq!(aggregate.savings_by_role[0].role, "leader");
        assert!((aggregate.savings_by_role[0].estimated_savings_usd + 0.1).abs() < 1e-9);
        assert_eq!(aggregate.savings_by_role[1].role, "worker");
        assert!((aggregate.savings_by_role[1].estimated_savings_usd - 0.5).abs() < 1e-9);
    }

    #[test]
    fn aggregate_ignores_missing_fields_and_counts_compaction_resets() {
        let cache = vec![event("context.cache", serde_json::json!({}))];
        let compactions = vec![
            event(
                "context.compaction",
                serde_json::json!({
                    "cache_reset_reason": "compaction",
                    "cache_invalidation_tokens": 700,
                    "cache_invalidation_estimated_usd": 0.0063
                }),
            ),
            event("context.compaction", serde_json::json!({})),
        ];
        let aggregate = aggregate_cache_events(&cache, &compactions);
        assert_eq!(aggregate.calls, 1);
        assert_eq!(aggregate.warm_calls, 0);
        assert!(aggregate.miss_reason_counts.is_empty());
        assert!(aggregate.ttl_counts.is_empty());
        assert!(aggregate.breakpoint_counts.is_empty());
        assert_eq!(aggregate.savings_by_role.len(), 1);
        assert_eq!(aggregate.savings_by_role[0].role, "(unassigned)");
        assert_eq!(aggregate.first_token_p50_ms, None);
        assert_eq!(aggregate.compaction_resets, 1);
        assert_eq!(aggregate.compaction_invalidation_tokens, 700);
        assert!((aggregate.compaction_invalidation_estimated_usd - 0.0063).abs() < 1e-9);
    }

    #[test]
    fn nearest_rank_is_empty_safe_and_within_bounds() {
        assert_eq!(nearest_rank(&[], 50), None);
        assert_eq!(nearest_rank(&[10], 95), Some(10));
        assert_eq!(nearest_rank(&[10, 20, 30, 40], 50), Some(20));
        assert_eq!(nearest_rank(&[10, 20, 30, 40], 95), Some(40));
        assert_eq!(nearest_rank(&[10, 20, 30, 40], 100), Some(40));
    }

    #[test]
    fn same_epoch_low_hit_is_a_provider_miss_not_a_prefix_change() {
        let mut tracker = CacheCallTracker::default();
        let epoch = "a".repeat(64);
        let current = sections()
            .hashes("model", &[])
            .fingerprint_with_epoch(&epoch);
        assert_eq!(
            tracker.observe_call(
                &current,
                CacheCallObservation {
                    input_tokens: 1_000,
                    cache_read_tokens: 400,
                    first_session_call: true,
                    prior_sessions: 0,
                }
            ),
            Some(CacheMissReason::FirstCall),
            "a brand-new epoch with a partial cold hit is a first call"
        );
        assert_eq!(
            tracker.observe_call(
                &current,
                CacheCallObservation {
                    input_tokens: 1_000,
                    cache_read_tokens: 400,
                    first_session_call: false,
                    prior_sessions: 0,
                }
            ),
            Some(CacheMissReason::ProviderCacheMiss),
            "the same epoch must not be reported as our prefix changing"
        );
        assert_eq!(
            tracker.observe_call(
                &current,
                CacheCallObservation {
                    input_tokens: 1_000,
                    cache_read_tokens: 900,
                    first_session_call: false,
                    prior_sessions: 0,
                }
            ),
            None,
            "a normal hit is not a miss"
        );
    }

    #[test]
    fn known_epoch_with_zero_reads_is_a_provider_cache_miss() {
        let mut tracker = CacheCallTracker::default();
        let epoch = "b".repeat(64);
        let current = sections()
            .hashes("model", &[])
            .fingerprint_with_epoch(&epoch);
        assert_eq!(
            tracker.observe_call(
                &current,
                CacheCallObservation {
                    input_tokens: 1_000,
                    cache_read_tokens: 0,
                    first_session_call: true,
                    prior_sessions: 3,
                }
            ),
            Some(CacheMissReason::ProviderCacheMiss),
            "the epoch is known in the project, so a cold call is provider eviction"
        );
    }

    #[test]
    fn epoch_change_with_same_sections_is_not_a_provider_miss() {
        let mut tracker = CacheCallTracker::default();
        let first = sections()
            .hashes("model-a", &[])
            .fingerprint_with_epoch_and_model(&"a".repeat(64), "model-a");
        tracker.observe_call(
            &first,
            CacheCallObservation {
                input_tokens: 1_000,
                cache_read_tokens: 900,
                first_session_call: true,
                prior_sessions: 0,
            },
        );
        let second = sections()
            .hashes("model-b", &[])
            .fingerprint_with_epoch_and_model(&"b".repeat(64), "model-b");
        assert_eq!(
            tracker.observe_call(
                &second,
                CacheCallObservation {
                    input_tokens: 1_000,
                    cache_read_tokens: 0,
                    first_session_call: false,
                    prior_sessions: 0,
                }
            ),
            Some(CacheMissReason::ModelChanged),
            "a model switch is a new epoch, not provider eviction"
        );
    }

    #[test]
    fn aggregate_counts_epoch_reuse_and_provider_misses() {
        let events = vec![
            vp_protocol::types::Event {
                session_id: "ses".into(),
                seq: 1,
                kind: "context.cache".into(),
                payload: serde_json::json!({
                    "input_tokens": 1_000,
                    "cache_read_tokens": 900,
                    "warm": false,
                    "cache_epoch": "a".repeat(64),
                    "cross_session_first_call": true,
                    "cross_session_cache_read_tokens": 900,
                    "miss_reason": null,
                }),
                created_at: "2026-11-12T00:00:01Z".into(),
            },
            vp_protocol::types::Event {
                session_id: "ses".into(),
                seq: 2,
                kind: "context.cache".into(),
                payload: serde_json::json!({
                    "input_tokens": 1_000,
                    "cache_read_tokens": 100,
                    "warm": false,
                    "cache_epoch": "a".repeat(64),
                    "miss_reason": "provider_cache_miss",
                }),
                created_at: "2026-11-12T00:00:02Z".into(),
            },
        ];
        let aggregate = aggregate_cache_events(&events, &[]);
        assert_eq!(aggregate.calls, 2);
        assert_eq!(aggregate.cross_session_hit_tokens, 900);
        assert_eq!(aggregate.cross_session_first_calls, 1);
        assert_eq!(aggregate.provider_cache_misses, 1);
        assert_eq!(aggregate.prefix_changes, 0);
        assert_eq!(aggregate.cache_epoch_counts.len(), 1);
        assert_eq!(aggregate.cache_epoch_counts[0].calls, 2);
    }

    #[test]
    fn aggregate_reads_l4_reset_events_and_legacy_compaction_fields() {
        let reset = vp_protocol::types::Event {
            session_id: "ses".into(),
            seq: 3,
            kind: "context.compaction.cache_reset".into(),
            payload: serde_json::json!({
                "reset_reason": "compaction",
                "invalidated_tokens": 4_000,
                "estimated_lost_cache_read_usd": 0.01,
            }),
            created_at: "2026-11-12T00:00:03Z".into(),
        };
        let legacy = vp_protocol::types::Event {
            session_id: "ses".into(),
            seq: 4,
            kind: "context.compaction".into(),
            payload: serde_json::json!({
                "cache_reset_reason": "compaction",
                "cache_invalidation_tokens": 2_000,
                "cache_invalidation_estimated_usd": 0.005,
            }),
            created_at: "2026-11-12T00:00:04Z".into(),
        };
        let with_reset = aggregate_cache_events(&[], std::slice::from_ref(&reset));
        assert_eq!(with_reset.compaction_resets, 1);
        assert_eq!(with_reset.compaction_invalidation_tokens, 4_000);
        assert!((with_reset.compaction_lost_cache_usd - 0.01).abs() < 1e-9);
        let with_legacy = aggregate_cache_events(&[], std::slice::from_ref(&legacy));
        assert_eq!(with_legacy.compaction_resets, 1);
        assert_eq!(with_legacy.compaction_invalidation_tokens, 2_000);
        assert!((with_legacy.compaction_lost_cache_usd - 0.005).abs() < 1e-9);
    }

    #[test]
    fn miss_reason_strings_round_trip() {
        for reason in [
            CacheMissReason::FirstCall,
            CacheMissReason::PrefixChanged,
            CacheMissReason::ModelChanged,
            CacheMissReason::ToolsChanged,
            CacheMissReason::ProjectChanged,
            CacheMissReason::ProviderCacheMiss,
            CacheMissReason::TtlOrProviderMiss,
            CacheMissReason::CompactionReset,
            CacheMissReason::Unknown,
        ] {
            assert_eq!(CacheMissReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(CacheMissReason::parse("provider_said_no"), None);
    }
}

#[cfg(test)]
mod v3_tests {
    use super::*;
    use crate::instruction::{
        InstructionCandidate, InstructionSectionKey, PROMPT_LAYOUT_V3, preview,
    };
    use vp_providers::ToolSchema;

    fn tool(name: &str) -> ToolSchema {
        ToolSchema {
            name: name.to_string(),
            description: format!("{name} tool"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    fn v3_sections(memory: &str) -> PromptSections {
        let candidates = vec![
            InstructionCandidate::new(InstructionSectionKey::CoreCharter, "charter bytes"),
            InstructionCandidate::new(InstructionSectionKey::CoreRecipes, "recipe bytes"),
            InstructionCandidate::new(InstructionSectionKey::ProjectSources, "source bytes"),
            InstructionCandidate::new(InstructionSectionKey::ProjectSkills, "skill bytes"),
            InstructionCandidate::new(InstructionSectionKey::SessionContext, "session bytes"),
            InstructionCandidate::new(InstructionSectionKey::RunTask, "task bytes"),
            InstructionCandidate::new(InstructionSectionKey::TailMemory, memory),
        ];
        let materialized = preview("ses-1", &candidates, true).unwrap();
        PromptSections::from_materialized(&materialized)
    }

    #[test]
    fn v3_renders_head_only_and_keeps_tail_out_of_the_prefix() {
        let sections = v3_sections("memory bytes");
        assert!(sections.is_v3());
        assert_eq!(sections.layout, PROMPT_LAYOUT_V3);
        assert!(sections.render().contains("charter bytes"));
        assert!(sections.render().contains("task bytes"));
        assert!(!sections.render().contains("memory bytes"));
        assert_eq!(sections.stable_prefix(), sections.render());
        assert_eq!(sections.tail_sections().len(), 1);
        assert_eq!(sections.tail_sections()[0].kind, "tail/memory");
        assert_eq!(sections.tail_sections()[0].text, "memory bytes");
    }

    #[test]
    fn v3_hashing_excludes_memory_and_records_ordered_sections() {
        let tools = vec![tool("read_file")];
        let first = v3_sections("memory one").hashes("model", &tools);
        let second = v3_sections("memory two").hashes("model", &tools);
        assert_eq!(first.prompt_layout, 3);
        assert_eq!(first.prefix_hash, second.prefix_hash);
        assert_eq!(first.stable_prefix_hash, second.stable_prefix_hash);
        assert_eq!(first.head_sections, second.head_sections);
        assert_eq!(first.head_sections.len(), 6);
        assert_eq!(first.tail_sections.len(), 1);
        assert_eq!(first.tail_sections[0].key, "tail/memory");
        assert_ne!(first.volatile_hash, second.volatile_hash);
        assert_eq!(first.section_chars.get("core/charter"), Some(&13));
        assert_eq!(first.section_chars.get("tail/memory"), Some(&10));
        let keys = first
            .head_sections
            .iter()
            .map(|section| section.key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            keys,
            vec![
                "core/charter",
                "core/recipes",
                "project/sources",
                "project/skills",
                "session/context",
                "run/task",
            ]
        );
        assert!(first.head_sections.iter().all(|section| !section.tail));
        assert!(first.tail_sections.iter().all(|section| section.tail));
    }

    #[test]
    fn v3_project_change_moves_the_prefix_and_a_prompt_does_not() {
        let tools = vec![tool("read_file")];
        let base = v3_sections("memory");
        let mut changed_project = v3_sections("memory");
        changed_project.head[2].text = "different source bytes".to_string();
        let base_hash = base.hashes("model", &tools);
        let project_hash = changed_project.hashes("model", &tools);
        assert_ne!(base_hash.prefix_hash, project_hash.prefix_hash);
        assert_eq!(base_hash.head_sections[0], project_hash.head_sections[0]);

        // A user prompt is not represented in PromptSections at all; adding a
        // user message cannot change the head hash.
        let messages_before = vec![ChatMessage::user("first prompt")];
        let messages_after = vec![
            ChatMessage::user("first prompt"),
            ChatMessage::assistant_tool_calls("", Vec::new()),
            ChatMessage::user("second prompt"),
        ];
        assert_eq!(
            conversation_generation(&base_hash.prefix_hash, &messages_before),
            conversation_generation(&base_hash.prefix_hash, &messages_before)
        );
        assert_ne!(
            conversation_generation(&base_hash.prefix_hash, &messages_before),
            conversation_generation(&base_hash.prefix_hash, &messages_after)
        );
        assert_eq!(
            base_hash.prefix_hash,
            base.hashes("model", &tools).prefix_hash
        );
    }

    #[test]
    fn v3_provider_sections_keep_one_core_and_one_project_breakpoint_kind() {
        let sections = v3_sections("memory");
        let provider = sections.provider_sections();
        assert_eq!(provider.len(), 6);
        assert_eq!(
            provider
                .iter()
                .filter(|section| section.kind == "core")
                .count(),
            1
        );
        assert_eq!(
            provider
                .iter()
                .filter(|section| section.kind == "project")
                .count(),
            1
        );
        assert_eq!(provider.last().unwrap().kind, "task");
        let policy = CachePolicy::default();
        let plan = request_cache_plan("anthropic", &sections, 12, 4, &policy, "leader");
        assert_eq!(plan.breakpoints, 4);
        assert!(plan.core_breakpoint);
        assert!(plan.project_breakpoint);
        let openai = request_cache_plan("openai-compatible", &sections, 12, 4, &policy, "leader");
        assert_eq!(openai.breakpoints, 0);
    }

    #[test]
    fn v2_hashing_keeps_the_legacy_prefix_identity() {
        let sections = PromptSections::new("core", "project", "volatile");
        let hashes = sections.hashes("model", &[tool("read_file")]);
        assert_eq!(hashes.prompt_layout, 2);
        assert_eq!(hashes.head_sections.len(), 2);
        assert!(hashes.tail_sections.is_empty());
        assert_eq!(hashes.stable_prefix_hash, hashes.stable_system_hash);
        assert_eq!(
            hashes.prefix_hash,
            prefix_hash_from_parts(
                "model",
                &hashes.tools_hash,
                &hashes.core_hash,
                &hashes.project_hash
            )
        );
    }
}

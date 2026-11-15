//! Bounded A2A 1.0-shaped wire model.
//!
//! These types are intentionally transport-neutral. The facade never executes
//! a part, dereferences a file URI, or treats `data` as code. Validation is
//! explicit at every trust boundary so `serde` alone cannot admit an
//! over-sized or non-loopback Agent Card.

use std::collections::BTreeSet;
use std::fmt;

use base64::Engine as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::error::{A2aError, A2aResult};
use crate::limits::{A2aLimits, MAX_ID_BYTES, MAX_METADATA_BYTES, MAX_OUTPUT_MODES, MAX_URL_BYTES};
use crate::url::validate_loopback_http_url;

pub const AGENT_PROTOCOL_VERSION: &str = "1.0";
const MAX_NAME_BYTES: usize = 256;
const MAX_DESCRIPTION_BYTES: usize = 8 * 1024;
const MAX_VERSION_BYTES: usize = 64;
const MAX_PROTOCOL_VERSION_BYTES: usize = 32;
const MAX_SKILL_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_TAG_BYTES: usize = 64;
const MAX_EXAMPLE_BYTES: usize = 512;
const MAX_MODE_BYTES: usize = 128;
const MAX_TAGS_PER_SKILL: usize = 16;
const MAX_EXAMPLES_PER_SKILL: usize = 8;
const MAX_PROVIDER_ORGANIZATION_BYTES: usize = 256;
const MAX_SECURITY_SCHEMES: usize = 16;
const MAX_SECURITY_SCHEME_BYTES: usize = 4 * 1024;
const MAX_SECURITY_SCHEMES_BYTES: usize = 32 * 1024;
const MAX_EXTENSIONS: usize = 16;
const MAX_EXTENSION_DESCRIPTION_BYTES: usize = 1024;
const MAX_EXTENSION_PARAMS_BYTES: usize = 16 * 1024;
const MAX_EXTENSIONS_BYTES: usize = 64 * 1024;
const MAX_ARTIFACT_NAME_BYTES: usize = 256;
const MAX_ARTIFACT_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_TIMESTAMP_BYTES: usize = 64;
const MAX_FILE_NAME_BYTES: usize = 256;
const MAX_MIME_TYPE_BYTES: usize = 128;

/// Capability flags advertised by an agent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub push_notifications: bool,
    #[serde(default)]
    pub state_transition_history: bool,
}

/// A skill advertised on the Agent Card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

impl AgentSkill {
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            description: description.into(),
            tags: Vec::new(),
            examples: Vec::new(),
            input_modes: Vec::new(),
            output_modes: Vec::new(),
        }
    }

    fn validate(&self) -> A2aResult<()> {
        validate_id("skill.id", &self.id)?;
        require_text("skill.name", &self.name, MAX_NAME_BYTES)?;
        if self.description.len() > MAX_SKILL_DESCRIPTION_BYTES {
            return Err(A2aError::BoundsExceeded {
                field: "skill.description",
                max: MAX_SKILL_DESCRIPTION_BYTES,
                actual: self.description.len(),
            });
        }
        if self.tags.len() > MAX_TAGS_PER_SKILL {
            return Err(A2aError::BoundsExceeded {
                field: "skill.tags",
                max: MAX_TAGS_PER_SKILL,
                actual: self.tags.len(),
            });
        }
        for tag in &self.tags {
            if tag.len() > MAX_TAG_BYTES || tag.bytes().any(|b| b.is_ascii_control()) {
                return Err(A2aError::InvalidParams {
                    reason: "skill tag is empty, too long, or contains control bytes".into(),
                });
            }
        }
        if self.examples.len() > MAX_EXAMPLES_PER_SKILL {
            return Err(A2aError::BoundsExceeded {
                field: "skill.examples",
                max: MAX_EXAMPLES_PER_SKILL,
                actual: self.examples.len(),
            });
        }
        for example in &self.examples {
            if example.len() > MAX_EXAMPLE_BYTES || example.bytes().any(|b| b.is_ascii_control()) {
                return Err(A2aError::BoundsExceeded {
                    field: "skill.example",
                    max: MAX_EXAMPLE_BYTES,
                    actual: example.len(),
                });
            }
        }
        validate_modes("skill.inputModes", &self.input_modes)?;
        validate_modes("skill.outputModes", &self.output_modes)?;
        Ok(())
    }
}

/// Provider metadata is descriptive only; it is never fetched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProvider {
    pub organization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl AgentProvider {
    pub fn new(organization: impl Into<String>) -> Self {
        Self {
            organization: organization.into(),
            url: None,
        }
    }

    fn validate(&self) -> A2aResult<()> {
        require_text(
            "provider.organization",
            &self.organization,
            MAX_PROVIDER_ORGANIZATION_BYTES,
        )?;
        if let Some(url) = &self.url
            && (url.is_empty() || url.len() > MAX_URL_BYTES || has_control(url))
        {
            return Err(A2aError::InvalidParams {
                reason: "provider URL is invalid".into(),
            });
        }
        Ok(())
    }
}

/// An Agent Card extension descriptor.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExtension {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Map<String, Value>>,
}

impl AgentExtension {
    fn validate(&self) -> A2aResult<()> {
        if self.uri.is_empty() || self.uri.len() > MAX_URL_BYTES || has_control(&self.uri) {
            return Err(A2aError::InvalidParams {
                reason: "extension URI is invalid".into(),
            });
        }
        if let Some(description) = &self.description
            && description.len() > MAX_EXTENSION_DESCRIPTION_BYTES
        {
            return Err(A2aError::BoundsExceeded {
                field: "extension.description",
                max: MAX_EXTENSION_DESCRIPTION_BYTES,
                actual: description.len(),
            });
        }
        if let Some(params) = &self.params {
            let bytes = serde_json::to_vec(params).map_err(|_| A2aError::Internal)?;
            if bytes.len() > MAX_EXTENSION_PARAMS_BYTES {
                return Err(A2aError::BoundsExceeded {
                    field: "extension.params",
                    max: MAX_EXTENSION_PARAMS_BYTES,
                    actual: bytes.len(),
                });
            }
        }
        Ok(())
    }
}

/// The discovery document served at `/.well-known/agent-card.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub url: String,
    pub version: String,
    pub protocol_version: String,
    pub capabilities: AgentCapabilities,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<AgentProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub security_schemes: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<AgentExtension>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentCardWire {
    name: String,
    description: String,
    url: String,
    version: String,
    protocol_version: String,
    #[serde(default)]
    capabilities: AgentCapabilities,
    #[serde(default)]
    default_input_modes: Vec<String>,
    #[serde(default)]
    default_output_modes: Vec<String>,
    #[serde(default)]
    skills: Vec<AgentSkill>,
    #[serde(default)]
    provider: Option<AgentProvider>,
    #[serde(default)]
    security_schemes: Option<Map<String, Value>>,
    #[serde(default)]
    extensions: Option<Vec<AgentExtension>>,
}

impl From<AgentCardWire> for AgentCard {
    fn from(wire: AgentCardWire) -> Self {
        Self {
            name: wire.name,
            description: wire.description,
            url: wire.url,
            version: wire.version,
            protocol_version: wire.protocol_version,
            capabilities: wire.capabilities,
            default_input_modes: wire.default_input_modes,
            default_output_modes: wire.default_output_modes,
            skills: wire.skills,
            provider: wire.provider,
            security_schemes: wire.security_schemes,
            extensions: wire.extensions,
        }
    }
}

impl<'de> Deserialize<'de> for AgentCard {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AgentCardWire::deserialize(deserializer)?;
        let card = AgentCard::from(wire);
        card.validate(&A2aLimits::default())
            .map_err(|error| D::Error::custom(error.public_message()))?;
        Ok(card)
    }
}

impl AgentCard {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        url: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            url: url.into(),
            version: version.into(),
            protocol_version: AGENT_PROTOCOL_VERSION.into(),
            capabilities: AgentCapabilities::default(),
            default_input_modes: Vec::new(),
            default_output_modes: Vec::new(),
            skills: Vec::new(),
            provider: None,
            security_schemes: None,
            extensions: None,
        }
    }

    /// Decode raw JSON and apply the caller's typed bounds.
    pub fn decode(bytes: &[u8], limits: &A2aLimits) -> A2aResult<Self> {
        if bytes.len() > limits.max_body_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "agentCard",
                max: limits.max_body_bytes,
                actual: bytes.len(),
            });
        }
        let wire: AgentCardWire =
            serde_json::from_slice(bytes).map_err(|error| A2aError::AgentCardInvalid {
                reason: error.to_string(),
            })?;
        let card = Self::from(wire);
        card.validate(limits)?;
        Ok(card)
    }

    /// Encode after validating against the canonical limits.
    pub fn encode(&self) -> A2aResult<String> {
        self.validate(&A2aLimits::default())?;
        serde_json::to_string(self).map_err(|_| A2aError::Internal)
    }

    /// Alias for [`AgentCard::encode`].
    pub fn to_json(&self) -> A2aResult<String> {
        self.encode()
    }

    /// Decode with the default canonical limits.
    pub fn from_json(bytes: &[u8]) -> A2aResult<Self> {
        Self::decode(bytes, &A2aLimits::default())
    }

    /// Validate the card, including its literal-loopback URL.
    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        require_text("agentCard.name", &self.name, MAX_NAME_BYTES)?;
        if self.description.len() > MAX_DESCRIPTION_BYTES {
            return Err(A2aError::BoundsExceeded {
                field: "agentCard.description",
                max: MAX_DESCRIPTION_BYTES,
                actual: self.description.len(),
            });
        }
        if self.version.is_empty() || self.version.len() > MAX_VERSION_BYTES {
            return Err(A2aError::InvalidParams {
                reason: "agentCard.version is empty or too long".into(),
            });
        }
        if self.protocol_version.is_empty()
            || self.protocol_version.len() > MAX_PROTOCOL_VERSION_BYTES
        {
            return Err(A2aError::InvalidParams {
                reason: "agentCard.protocolVersion is empty or too long".into(),
            });
        }
        // This is the key loopback invariant: a decoded card can never point
        // at a public or hostname-based endpoint.
        validate_loopback_http_url(&self.url)?;
        validate_modes("agentCard.defaultInputModes", &self.default_input_modes)?;
        validate_modes("agentCard.defaultOutputModes", &self.default_output_modes)?;
        if self.skills.len() > limits.max_skills {
            return Err(A2aError::BoundsExceeded {
                field: "skills",
                max: limits.max_skills,
                actual: self.skills.len(),
            });
        }
        let mut ids = BTreeSet::new();
        for skill in &self.skills {
            skill.validate()?;
            if !ids.insert(skill.id.clone()) {
                return Err(A2aError::AgentCardInvalid {
                    reason: "agent card contains duplicate skill ids".into(),
                });
            }
        }
        if let Some(provider) = &self.provider {
            provider.validate()?;
        }
        if let Some(schemes) = &self.security_schemes {
            if schemes.len() > MAX_SECURITY_SCHEMES {
                return Err(A2aError::BoundsExceeded {
                    field: "securitySchemes",
                    max: MAX_SECURITY_SCHEMES,
                    actual: schemes.len(),
                });
            }
            let mut total = 0usize;
            for (name, scheme) in schemes {
                validate_id("securityScheme.name", name)?;
                if !scheme.is_object() {
                    return Err(A2aError::AgentCardInvalid {
                        reason: "security schemes must be JSON objects".into(),
                    });
                }
                let bytes = serde_json::to_vec(scheme).map_err(|_| A2aError::Internal)?;
                if bytes.len() > MAX_SECURITY_SCHEME_BYTES {
                    return Err(A2aError::BoundsExceeded {
                        field: "securityScheme",
                        max: MAX_SECURITY_SCHEME_BYTES,
                        actual: bytes.len(),
                    });
                }
                total = total.saturating_add(bytes.len());
            }
            if total > MAX_SECURITY_SCHEMES_BYTES {
                return Err(A2aError::BoundsExceeded {
                    field: "securitySchemes",
                    max: MAX_SECURITY_SCHEMES_BYTES,
                    actual: total,
                });
            }
        }
        if let Some(extensions) = &self.extensions {
            if extensions.len() > MAX_EXTENSIONS {
                return Err(A2aError::BoundsExceeded {
                    field: "extensions",
                    max: MAX_EXTENSIONS,
                    actual: extensions.len(),
                });
            }
            let mut total = 0usize;
            for extension in extensions {
                extension.validate()?;
                total = total.saturating_add(extension.uri.len());
            }
            let bytes = serde_json::to_vec(extensions).map_err(|_| A2aError::Internal)?;
            if bytes.len() > MAX_EXTENSIONS_BYTES {
                return Err(A2aError::BoundsExceeded {
                    field: "extensions",
                    max: MAX_EXTENSIONS_BYTES,
                    actual: bytes.len(),
                });
            }
            let _ = total;
        }
        let bytes = serde_json::to_vec(self).map_err(|_| A2aError::Internal)?;
        if bytes.len() > limits.max_body_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "agentCard",
                max: limits.max_body_bytes,
                actual: bytes.len(),
            });
        }
        Ok(())
    }

    /// Convenience constructor used by the server's policy projection.
    pub fn with_capabilities(mut self, capabilities: AgentCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }
}

/// Message participant role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    User,
    Agent,
}

/// A file attached to a message or artifact. Exactly one of `bytes` or `uri`
/// is present. The facade never fetches a URI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

impl FileContent {
    pub fn from_bytes(name: Option<String>, mime_type: Option<String>, bytes: &[u8]) -> Self {
        Self {
            name,
            mime_type,
            bytes: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
            uri: None,
        }
    }

    pub fn from_uri(
        name: Option<String>,
        mime_type: Option<String>,
        uri: impl Into<String>,
    ) -> Self {
        Self {
            name,
            mime_type,
            bytes: None,
            uri: Some(uri.into()),
        }
    }

    /// Decode inline bytes. URI-backed files return `Ok(None)`.
    pub fn decoded_bytes(&self) -> A2aResult<Option<Vec<u8>>> {
        match &self.bytes {
            Some(bytes) => base64::engine::general_purpose::STANDARD
                .decode(bytes)
                .map(Some)
                .map_err(|_| A2aError::InvalidParams {
                    reason: "file part is not valid base64".into(),
                }),
            None => Ok(None),
        }
    }

    fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        match (&self.bytes, &self.uri) {
            (Some(_), Some(_)) | (None, None) => {
                return Err(A2aError::InvalidParams {
                    reason: "file part must contain exactly one of bytes or uri".into(),
                });
            }
            _ => {}
        }
        if let Some(name) = &self.name
            && (name.is_empty() || name.len() > MAX_FILE_NAME_BYTES || has_control(name))
        {
            return Err(A2aError::InvalidParams {
                reason: "file name is invalid".into(),
            });
        }
        if let Some(mime_type) = &self.mime_type
            && (mime_type.is_empty()
                || mime_type.len() > MAX_MIME_TYPE_BYTES
                || mime_type.bytes().any(|b| b.is_ascii_control()))
        {
            return Err(A2aError::InvalidParams {
                reason: "file MIME type is invalid".into(),
            });
        }
        if let Some(uri) = &self.uri
            && (uri.is_empty() || uri.len() > MAX_URL_BYTES || has_control(uri))
        {
            return Err(A2aError::InvalidParams {
                reason: "file URI is invalid".into(),
            });
        }
        if let Some(bytes) = self.decoded_bytes()?
            && bytes.len() > limits.max_file_part_bytes
        {
            return Err(A2aError::BoundsExceeded {
                field: "message.part.file",
                max: limits.max_file_part_bytes,
                actual: bytes.len(),
            });
        }
        Ok(())
    }
}

/// A content part. The `kind` discriminator is serialized in lowercase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Part {
    Text { text: String },
    Data { data: Map<String, Value> },
    File { file: FileContent },
}

impl Part {
    pub fn text(value: impl Into<String>) -> Self {
        Self::Text { text: value.into() }
    }

    pub fn data(value: Map<String, Value>) -> Self {
        Self::Data { data: value }
    }

    pub fn file(value: FileContent) -> Self {
        Self::File { file: value }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Text { .. } => "text",
            Self::Data { .. } => "data",
            Self::File { .. } => "file",
        }
    }

    fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        match self {
            Self::Text { text } => {
                if text.len() > limits.max_part_text_bytes {
                    return Err(A2aError::BoundsExceeded {
                        field: "message.part.text",
                        max: limits.max_part_text_bytes,
                        actual: text.len(),
                    });
                }
            }
            Self::Data { data } => {
                let bytes = serde_json::to_vec(data).map_err(|_| A2aError::Internal)?;
                if bytes.len() > limits.max_data_bytes {
                    return Err(A2aError::BoundsExceeded {
                        field: "message.part.data",
                        max: limits.max_data_bytes,
                        actual: bytes.len(),
                    });
                }
            }
            Self::File { file } => file.validate(limits)?,
        }
        Ok(())
    }
}

/// A message exchanged with an A2A agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: MessageRole,
    pub parts: Vec<Part>,
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default = "message_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

fn message_kind() -> String {
    "message".into()
}

impl Message {
    pub fn new(role: MessageRole, message_id: impl Into<String>, parts: Vec<Part>) -> Self {
        Self {
            role,
            parts,
            message_id: message_id.into(),
            context_id: None,
            task_id: None,
            kind: message_kind(),
            metadata: None,
        }
    }

    pub fn user_text(message_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(MessageRole::User, message_id, vec![Part::text(text)])
    }

    pub fn agent_text(message_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(MessageRole::Agent, message_id, vec![Part::text(text)])
    }

    pub fn with_context(mut self, context_id: impl Into<String>) -> Self {
        self.context_id = Some(context_id.into());
        self
    }

    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        if self.kind != "message" {
            return Err(A2aError::InvalidParams {
                reason: "message.kind must be \"message\"".into(),
            });
        }
        validate_id("message.messageId", &self.message_id)?;
        if let Some(context_id) = &self.context_id {
            validate_id("message.contextId", context_id)?;
        }
        if let Some(task_id) = &self.task_id {
            validate_id("message.taskId", task_id)?;
        }
        if self.parts.is_empty() {
            return Err(A2aError::InvalidParams {
                reason: "message must contain at least one part".into(),
            });
        }
        if self.parts.len() > limits.max_message_parts {
            return Err(A2aError::BoundsExceeded {
                field: "message.parts",
                max: limits.max_message_parts,
                actual: self.parts.len(),
            });
        }
        for part in &self.parts {
            part.validate(limits)?;
        }
        validate_metadata(&self.metadata, "message.metadata")?;
        let bytes = serde_json::to_vec(self).map_err(|_| A2aError::Internal)?;
        if bytes.len() > limits.max_body_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "message",
                max: limits.max_body_bytes,
                actual: bytes.len(),
            });
        }
        Ok(())
    }
}

/// An artifact produced by a task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
    pub artifact_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl Artifact {
    pub fn new(artifact_id: impl Into<String>, parts: Vec<Part>) -> Self {
        Self {
            artifact_id: artifact_id.into(),
            name: None,
            description: None,
            parts,
            metadata: None,
        }
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        validate_id("artifact.artifactId", &self.artifact_id)?;
        if let Some(name) = &self.name
            && name.len() > MAX_ARTIFACT_NAME_BYTES
        {
            return Err(A2aError::BoundsExceeded {
                field: "artifact.name",
                max: MAX_ARTIFACT_NAME_BYTES,
                actual: name.len(),
            });
        }
        if let Some(description) = &self.description
            && description.len() > MAX_ARTIFACT_DESCRIPTION_BYTES
        {
            return Err(A2aError::BoundsExceeded {
                field: "artifact.description",
                max: MAX_ARTIFACT_DESCRIPTION_BYTES,
                actual: description.len(),
            });
        }
        if self.parts.is_empty() {
            return Err(A2aError::InvalidParams {
                reason: "artifact must contain at least one part".into(),
            });
        }
        if self.parts.len() > limits.max_message_parts {
            return Err(A2aError::BoundsExceeded {
                field: "artifact.parts",
                max: limits.max_message_parts,
                actual: self.parts.len(),
            });
        }
        for part in &self.parts {
            part.validate(limits)?;
        }
        validate_metadata(&self.metadata, "artifact.metadata")?;
        let bytes = serde_json::to_vec(self).map_err(|_| A2aError::Internal)?;
        if bytes.len() > limits.max_artifact_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "artifact",
                max: limits.max_artifact_bytes,
                actual: bytes.len(),
            });
        }
        Ok(())
    }
}

/// Task lifecycle state. Serialized with underscores, matching the facade's
/// canonical vocabulary; A2A's hyphenated spelling is accepted on input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum TaskState {
    #[default]
    Submitted,
    Working,
    #[serde(alias = "input-required")]
    InputRequired,
    #[serde(alias = "auth-required")]
    AuthRequired,
    Completed,
    Failed,
    #[serde(alias = "cancelled")]
    Canceled,
    Rejected,
}

impl TaskState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::InputRequired => "input_required",
            Self::AuthRequired => "auth_required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Rejected => "rejected",
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }

    /// Terminal or waiting for another participant. A settled task has no
    /// active handler run.
    pub const fn is_settled(self) -> bool {
        self.is_terminal() || matches!(self, Self::InputRequired | Self::AuthRequired)
    }

    pub const fn accepts_followup(self) -> bool {
        matches!(self, Self::InputRequired | Self::AuthRequired)
    }

    /// Deterministic state-machine check. Same-state transitions are allowed
    /// so a no-op or idempotent same-outcome can be recognized separately.
    pub const fn can_transition_to(self, next: Self) -> bool {
        // Same-state is a no-op; for a terminal state it is only the
        // idempotent same-outcome case, never a mutation.
        if matches!(
            (self, next),
            (Self::Submitted, Self::Submitted)
                | (Self::Working, Self::Working)
                | (Self::InputRequired, Self::InputRequired)
                | (Self::AuthRequired, Self::AuthRequired)
                | (Self::Completed, Self::Completed)
                | (Self::Failed, Self::Failed)
                | (Self::Canceled, Self::Canceled)
                | (Self::Rejected, Self::Rejected)
        ) {
            return true;
        }
        if self.is_terminal() {
            return false;
        }
        match self {
            Self::Submitted => matches!(
                next,
                Self::Working
                    | Self::InputRequired
                    | Self::AuthRequired
                    | Self::Completed
                    | Self::Failed
                    | Self::Canceled
                    | Self::Rejected
            ),
            Self::Working => matches!(
                next,
                Self::InputRequired
                    | Self::AuthRequired
                    | Self::Completed
                    | Self::Failed
                    | Self::Canceled
                    | Self::Rejected
            ),
            Self::InputRequired | Self::AuthRequired => matches!(
                next,
                Self::Working | Self::Completed | Self::Failed | Self::Canceled
            ),
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected => false,
        }
    }
}

impl fmt::Display for TaskState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Current task status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl TaskStatus {
    pub fn new(state: TaskState) -> Self {
        Self {
            state,
            message: None,
            timestamp: None,
        }
    }

    pub fn with_message(mut self, message: Message) -> Self {
        self.message = Some(message);
        self
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        if let Some(message) = &self.message {
            message.validate(limits)?;
        }
        if let Some(timestamp) = &self.timestamp
            && (timestamp.is_empty()
                || timestamp.len() > MAX_TIMESTAMP_BYTES
                || has_control(timestamp))
        {
            return Err(A2aError::InvalidParams {
                reason: "task status timestamp is invalid".into(),
            });
        }
        Ok(())
    }
}

/// A task projection returned by `tasks/get` and message methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aTask {
    pub id: String,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default = "task_kind")]
    pub kind: String,
}

fn task_kind() -> String {
    "task".into()
}

impl A2aTask {
    pub fn new(id: impl Into<String>, context_id: impl Into<String>, state: TaskState) -> Self {
        Self {
            id: id.into(),
            context_id: context_id.into(),
            status: TaskStatus::new(state),
            history: Vec::new(),
            artifacts: Vec::new(),
            metadata: None,
            kind: task_kind(),
        }
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        if self.kind != "task" {
            return Err(A2aError::InvalidParams {
                reason: "task.kind must be \"task\"".into(),
            });
        }
        validate_id("task.id", &self.id)?;
        validate_id("task.contextId", &self.context_id)?;
        self.status.validate(limits)?;
        if self.history.len() > limits.max_task_history {
            return Err(A2aError::BoundsExceeded {
                field: "task.history",
                max: limits.max_task_history,
                actual: self.history.len(),
            });
        }
        let mut history_bytes = 0usize;
        for message in &self.history {
            message.validate(limits)?;
            history_bytes = history_bytes.saturating_add(
                serde_json::to_vec(message)
                    .map_err(|_| A2aError::Internal)?
                    .len(),
            );
        }
        if history_bytes > limits.max_task_history_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "task.history.bytes",
                max: limits.max_task_history_bytes,
                actual: history_bytes,
            });
        }
        if self.artifacts.len() > limits.max_artifacts_per_task {
            return Err(A2aError::BoundsExceeded {
                field: "task.artifacts",
                max: limits.max_artifacts_per_task,
                actual: self.artifacts.len(),
            });
        }
        let mut artifact_bytes = 0usize;
        for artifact in &self.artifacts {
            artifact.validate(limits)?;
            artifact_bytes = artifact_bytes.saturating_add(
                serde_json::to_vec(artifact)
                    .map_err(|_| A2aError::Internal)?
                    .len(),
            );
        }
        if artifact_bytes > limits.max_task_artifact_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "task.artifacts.bytes",
                max: limits.max_task_artifact_bytes,
                actual: artifact_bytes,
            });
        }
        validate_metadata(&self.metadata, "task.metadata")?;
        Ok(())
    }

    pub fn with_history(mut self, history: Vec<Message>) -> Self {
        self.history = history;
        self
    }
}

/// What a host handler proposes after it has processed one run. `status` must
/// settle the run (`completed`, `failed`, `canceled`, `rejected`,
/// `input_required`, or `auth_required`) before the store will apply it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aTaskUpdate {
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
}

impl A2aTaskUpdate {
    pub fn new(state: TaskState) -> Self {
        Self {
            status: TaskStatus::new(state),
            messages: Vec::new(),
            artifacts: Vec::new(),
            metadata: None,
        }
    }

    pub fn completed() -> Self {
        Self::new(TaskState::Completed)
    }

    pub fn failed() -> Self {
        Self::new(TaskState::Failed)
    }

    pub fn input_required() -> Self {
        Self::new(TaskState::InputRequired)
    }

    pub fn auth_required() -> Self {
        Self::new(TaskState::AuthRequired)
    }

    pub fn with_status(mut self, status: TaskStatus) -> Self {
        self.status = status;
        self
    }

    pub fn with_message(mut self, message: Message) -> Self {
        self.messages.push(message);
        self
    }

    pub fn with_artifact(mut self, artifact: Artifact) -> Self {
        self.artifacts.push(artifact);
        self
    }

    pub fn with_metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        self.status.validate(limits)?;
        if self.messages.len() > limits.max_task_history {
            return Err(A2aError::BoundsExceeded {
                field: "update.messages",
                max: limits.max_task_history,
                actual: self.messages.len(),
            });
        }
        let mut history_bytes = 0usize;
        for message in &self.messages {
            message.validate(limits)?;
            history_bytes = history_bytes.saturating_add(
                serde_json::to_vec(message)
                    .map_err(|_| A2aError::Internal)?
                    .len(),
            );
        }
        if history_bytes > limits.max_task_history_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "update.messages.bytes",
                max: limits.max_task_history_bytes,
                actual: history_bytes,
            });
        }
        if self.artifacts.len() > limits.max_artifacts_per_task {
            return Err(A2aError::BoundsExceeded {
                field: "update.artifacts",
                max: limits.max_artifacts_per_task,
                actual: self.artifacts.len(),
            });
        }
        let mut artifact_bytes = 0usize;
        for artifact in &self.artifacts {
            artifact.validate(limits)?;
            artifact_bytes = artifact_bytes.saturating_add(
                serde_json::to_vec(artifact)
                    .map_err(|_| A2aError::Internal)?
                    .len(),
            );
        }
        if artifact_bytes > limits.max_task_artifact_bytes {
            return Err(A2aError::BoundsExceeded {
                field: "update.artifacts.bytes",
                max: limits.max_task_artifact_bytes,
                actual: artifact_bytes,
            });
        }
        validate_metadata(&self.metadata, "update.metadata")
    }
}

/// SSE status frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskStatusUpdateEvent {
    pub task_id: String,
    pub context_id: String,
    pub status: TaskStatus,
    #[serde(rename = "final")]
    pub is_final: bool,
    #[serde(default = "status_event_kind")]
    pub kind: String,
}

fn status_event_kind() -> String {
    "status-update".into()
}

impl TaskStatusUpdateEvent {
    pub fn new(
        task_id: impl Into<String>,
        context_id: impl Into<String>,
        status: TaskStatus,
        is_final: bool,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            context_id: context_id.into(),
            status,
            is_final,
            kind: status_event_kind(),
        }
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        if self.kind != "status-update" {
            return Err(A2aError::InvalidAgentResponse {
                reason: "status event has the wrong kind".into(),
            });
        }
        validate_id("event.taskId", &self.task_id)?;
        validate_id("event.contextId", &self.context_id)?;
        self.status.validate(limits)
    }
}

/// SSE artifact frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskArtifactUpdateEvent {
    pub task_id: String,
    pub context_id: String,
    pub artifact: Artifact,
    #[serde(default)]
    pub append: bool,
    #[serde(default)]
    pub last_chunk: bool,
    #[serde(default = "artifact_event_kind")]
    pub kind: String,
}

fn artifact_event_kind() -> String {
    "artifact-update".into()
}

impl TaskArtifactUpdateEvent {
    pub fn new(
        task_id: impl Into<String>,
        context_id: impl Into<String>,
        artifact: Artifact,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            context_id: context_id.into(),
            artifact,
            append: false,
            last_chunk: true,
            kind: artifact_event_kind(),
        }
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        if self.kind != "artifact-update" {
            return Err(A2aError::InvalidAgentResponse {
                reason: "artifact event has the wrong kind".into(),
            });
        }
        validate_id("event.taskId", &self.task_id)?;
        validate_id("event.contextId", &self.context_id)?;
        self.artifact.validate(limits)
    }
}

/// One of the two A2A streaming event shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum A2aStreamEvent {
    Status(TaskStatusUpdateEvent),
    Artifact(TaskArtifactUpdateEvent),
}

impl A2aStreamEvent {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Status(_) => "status-update",
            Self::Artifact(_) => "artifact-update",
        }
    }

    pub fn task_id(&self) -> &str {
        match self {
            Self::Status(event) => &event.task_id,
            Self::Artifact(event) => &event.task_id,
        }
    }

    pub fn context_id(&self) -> &str {
        match self {
            Self::Status(event) => &event.context_id,
            Self::Artifact(event) => &event.context_id,
        }
    }

    pub fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        match self {
            Self::Status(event) => event.validate(limits),
            Self::Artifact(event) => event.validate(limits),
        }
    }
}

/// A decoded SSE frame. `id` is the SSE `id:` value, distinct from the event
/// body; it is monotonically increasing within one task stream.
#[derive(Debug, Clone, PartialEq)]
pub struct A2aEventFrame {
    pub id: Option<u64>,
    pub event: A2aStreamEvent,
}

/// Type-level marker: the A2A wire uses camelCase in every JSON object.
pub(crate) fn validate_metadata(
    metadata: &Option<Map<String, Value>>,
    field: &'static str,
) -> A2aResult<()> {
    let Some(metadata) = metadata else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(metadata).map_err(|_| A2aError::Internal)?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(A2aError::BoundsExceeded {
            field,
            max: MAX_METADATA_BYTES,
            actual: bytes.len(),
        });
    }
    Ok(())
}

pub(crate) fn validate_id(field: &'static str, value: &str) -> A2aResult<()> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || has_control(value) {
        return Err(A2aError::InvalidParams {
            reason: format!("{field} must be 1..={MAX_ID_BYTES} bytes without control bytes"),
        });
    }
    Ok(())
}

fn require_text(field: &'static str, value: &str, max: usize) -> A2aResult<()> {
    if value.is_empty() || value.len() > max || has_control(value) {
        return Err(A2aError::InvalidParams {
            reason: format!("{field} must be 1..={max} bytes without control bytes"),
        });
    }
    Ok(())
}

fn validate_modes(field: &'static str, modes: &[String]) -> A2aResult<()> {
    if modes.len() > MAX_OUTPUT_MODES {
        return Err(A2aError::BoundsExceeded {
            field,
            max: MAX_OUTPUT_MODES,
            actual: modes.len(),
        });
    }
    for mode in modes {
        if mode.is_empty() || mode.len() > MAX_MODE_BYTES || has_control(mode) {
            return Err(A2aError::InvalidParams {
                reason: format!("{field} contains an invalid mode"),
            });
        }
    }
    Ok(())
}

fn has_control(value: &str) -> bool {
    value.bytes().any(|byte| byte.is_ascii_control())
}

// Re-exported for the policy card projection without making every field
// public in a second type.
pub(crate) fn default_input_modes() -> Vec<String> {
    vec!["text/plain".into(), "application/json".into()]
}

pub(crate) fn default_output_modes() -> Vec<String> {
    vec!["text/plain".into(), "application/json".into()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::{
        MAX_DATA_BYTES, MAX_MESSAGE_PARTS, MAX_PART_TEXT_BYTES, MAX_SKILLS, MAX_TASK_HISTORY,
    };

    fn loopback_card() -> AgentCard {
        AgentCard {
            name: "test agent".into(),
            description: "test".into(),
            url: "http://127.0.0.1:12345".into(),
            version: "1.0.0".into(),
            protocol_version: AGENT_PROTOCOL_VERSION.into(),
            capabilities: AgentCapabilities {
                streaming: true,
                push_notifications: false,
                state_transition_history: true,
            },
            default_input_modes: default_input_modes(),
            default_output_modes: default_output_modes(),
            skills: vec![AgentSkill::new("echo", "Echo", "Echoes input")],
            provider: Some(AgentProvider::new("VerlinPark")),
            security_schemes: None,
            extensions: None,
        }
    }

    #[test]
    fn agent_card_round_trips_and_rejects_public_urls() {
        let card = loopback_card();
        let encoded = card.encode().expect("encode");
        let decoded = AgentCard::decode(encoded.as_bytes(), &A2aLimits::default()).expect("decode");
        assert_eq!(decoded, card);
        let deserialized: AgentCard = serde_json::from_str(&encoded).expect("deserialize");
        assert_eq!(deserialized, card);

        let mut with_path = loopback_card();
        with_path.url = "http://127.0.0.1:12345/a2a".into();
        with_path
            .validate(&A2aLimits::default())
            .expect("loopback path is allowed on an advertised card");

        let mut public = card;
        public.url = "https://example.com:8443/a2a".into();
        let error = public
            .validate(&A2aLimits::default())
            .expect_err("public URL");
        assert_eq!(error.code(), crate::error::A2aErrorCode::LoopbackRequired);
        assert!(
            serde_json::from_value::<AgentCard>(serde_json::to_value(&public).expect("serialize"))
                .is_err()
        );
    }

    #[test]
    fn agent_card_bounds_skills() {
        let mut card = loopback_card();
        card.skills.clear();
        for index in 0..=MAX_SKILLS {
            card.skills
                .push(AgentSkill::new(format!("skill-{index}"), "Skill", ""));
        }
        let error = card
            .validate(&A2aLimits::default())
            .expect_err("too many skills");
        assert_eq!(error.code(), crate::error::A2aErrorCode::BoundsExceeded);
    }

    #[test]
    fn message_bounds_are_typed() {
        let limits = A2aLimits::default();
        let message = Message::user_text("m-1", "x".repeat(MAX_PART_TEXT_BYTES + 1));
        let error = message.validate(&limits).expect_err("oversized text");
        assert_eq!(error.code(), crate::error::A2aErrorCode::BoundsExceeded);

        let parts = (0..=MAX_MESSAGE_PARTS)
            .map(|index| Part::text(format!("part-{index}")))
            .collect();
        let message = Message::new(MessageRole::User, "m-2", parts);
        assert!(matches!(
            message.validate(&limits),
            Err(A2aError::BoundsExceeded { .. })
        ));
    }

    #[test]
    fn file_parts_require_exactly_one_source() {
        let limits = A2aLimits::default();
        let message = Message::new(
            MessageRole::User,
            "m-1",
            vec![Part::file(FileContent {
                name: None,
                mime_type: None,
                bytes: None,
                uri: None,
            })],
        );
        assert!(message.validate(&limits).is_err());

        let mut data = Map::new();
        data.insert("k".into(), Value::String("v".into()));
        let message = Message::new(
            MessageRole::User,
            "m-1",
            vec![Part::data(data), Part::text("hello")],
        );
        assert!(message.validate(&limits).is_ok());
    }

    #[test]
    fn data_artifact_and_history_bounds_are_typed() {
        let limits = A2aLimits::default();
        let mut data = Map::new();
        data.insert("blob".into(), Value::String("x".repeat(MAX_DATA_BYTES + 1)));
        let message = Message::new(MessageRole::User, "m-data", vec![Part::data(data)]);
        assert_eq!(
            message.validate(&limits).expect_err("data bound").code(),
            crate::error::A2aErrorCode::BoundsExceeded
        );

        let artifact = Artifact::new(
            "artifact-large",
            vec![
                Part::text("x".repeat(60 * 1024)),
                Part::text("y".repeat(60 * 1024)),
                Part::text("z".repeat(60 * 1024)),
                Part::text("w".repeat(60 * 1024)),
                Part::text("v".repeat(60 * 1024)),
            ],
        );
        assert_eq!(
            artifact
                .validate(&limits)
                .expect_err("artifact bound")
                .code(),
            crate::error::A2aErrorCode::BoundsExceeded
        );

        let mut task = A2aTask::new("task-1", "ctx-1", TaskState::Completed);
        for index in 0..=MAX_TASK_HISTORY {
            task.history
                .push(Message::user_text(format!("m-{index}"), "hi"));
        }
        assert_eq!(
            task.validate(&limits).expect_err("history bound").code(),
            crate::error::A2aErrorCode::BoundsExceeded
        );
    }

    #[test]
    fn task_state_transitions_are_deterministic() {
        assert!(TaskState::Submitted.can_transition_to(TaskState::Working));
        assert!(TaskState::Working.can_transition_to(TaskState::InputRequired));
        assert!(TaskState::InputRequired.can_transition_to(TaskState::Working));
        assert!(TaskState::Working.can_transition_to(TaskState::Completed));
        assert!(!TaskState::Completed.can_transition_to(TaskState::Working));
        assert!(TaskState::Completed.is_terminal());
        assert!(TaskState::InputRequired.is_settled());
        assert!(!TaskState::Working.is_settled());
    }

    #[test]
    fn task_status_accepts_both_hyphen_and_underscore_states() {
        let required: TaskState =
            serde_json::from_str("\"input-required\"").expect("hyphenated state");
        assert_eq!(required, TaskState::InputRequired);
        let wire = serde_json::to_string(&TaskState::InputRequired).expect("serialize");
        assert_eq!(wire, "\"input_required\"");
    }
}

//! Facade policy: opt-in, loopback bind, one bearer token, bounded card.

use std::fmt;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::error::{A2aError, A2aResult};
use crate::limits::A2aLimits;
use crate::url::{A2aListenAddr, A2aUrl, is_exact_loopback};
use crate::wire::{
    AGENT_PROTOCOL_VERSION, AgentCapabilities, AgentCard, AgentExtension, AgentProvider,
    AgentSkill, default_input_modes, default_output_modes,
};

/// A single bearer token that is redacted from `Debug`, never serialized, and
/// zeroized on drop. Plaintext is exposed only through crate-internal methods
/// and the one-time [`crate::server::A2aStartHandle::take_token`] accessor.
#[derive(Clone)]
pub struct A2aSecretToken(String);

impl A2aSecretToken {
    pub fn new(token: impl Into<String>) -> A2aResult<Self> {
        let token = token.into();
        if token.is_empty() || token.len() > 512 {
            return Err(A2aError::ConfigInvalid {
                reason: "bearer token must be 1..=512 bytes".into(),
            });
        }
        if token.bytes().any(|byte| {
            !(byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'='))
        }) {
            return Err(A2aError::ConfigInvalid {
                reason: "bearer token must use the RFC 6750 b64token character set".into(),
            });
        }
        Ok(Self(token))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Consume the token and return its plaintext to an explicit caller.
    pub(crate) fn into_string(mut self) -> String {
        std::mem::take(&mut self.0)
    }

    /// Very short tokens are still accepted for compatibility, but substring
    /// echo checks would produce false positives for ordinary words. Apply
    /// the additional echo guard only to tokens long enough to be meaningful
    /// secrets.
    pub(crate) fn echo_checked(&self) -> bool {
        self.0.len() >= 8
    }

    pub(crate) fn hash(&self) -> [u8; 32] {
        let digest = Sha256::digest(self.0.as_bytes());
        let mut output = [0u8; 32];
        output.copy_from_slice(&digest);
        output
    }

    /// Remove this token from remote text before it becomes an error. This is
    /// a last line of defence against a malicious peer echoing a header.
    pub(crate) fn redact(&self, text: &str) -> String {
        if text.contains(&self.0) {
            text.replace(&self.0, "[redacted]")
        } else {
            text.to_string()
        }
    }
}

impl fmt::Debug for A2aSecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("A2aSecretToken(<redacted>)")
    }
}

impl Drop for A2aSecretToken {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Compare two fixed-size hashes without an early exit. Both inputs are
/// SHA-256 digests, so their length is already public and fixed.
pub(crate) fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    let mut difference = 0u8;
    for index in 0..32 {
        difference |= left[index] ^ right[index];
    }
    std::hint::black_box(difference) == 0
}

/// Card metadata supplied by the host. The URL is deliberately not part of
/// this type: it is always derived from the bound loopback socket.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentCardSpec {
    pub name: String,
    pub description: String,
    pub version: String,
    pub protocol_version: String,
    pub capabilities: AgentCapabilities,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
    pub provider: Option<AgentProvider>,
    pub security_schemes: Option<Map<String, Value>>,
    pub extensions: Option<Vec<AgentExtension>>,
}

impl Default for AgentCardSpec {
    fn default() -> Self {
        Self {
            name: "VerlinPark A2A".into(),
            description: "Loopback-only Agent2Agent facade".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_version: AGENT_PROTOCOL_VERSION.into(),
            capabilities: AgentCapabilities {
                streaming: true,
                push_notifications: false,
                state_transition_history: true,
            },
            default_input_modes: default_input_modes(),
            default_output_modes: default_output_modes(),
            skills: Vec::new(),
            provider: None,
            security_schemes: None,
            extensions: None,
        }
    }
}

impl AgentCardSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            version: version.into(),
            ..Self::default()
        }
    }

    /// Build the served card. The URL is the actual bound loopback address
    /// and a bearer security-scheme entry is advertised automatically.
    pub fn card_at(&self, url: &A2aUrl) -> AgentCard {
        let mut schemes = self.security_schemes.clone().unwrap_or_default();
        schemes.insert(
            "bearerAuth".into(),
            json!({
                "type": "http",
                "scheme": "bearer",
                "bearerFormat": "opaque"
            }),
        );
        AgentCard {
            name: self.name.clone(),
            description: self.description.clone(),
            url: url.to_string(),
            version: self.version.clone(),
            protocol_version: self.protocol_version.clone(),
            capabilities: self.capabilities.clone(),
            default_input_modes: self.default_input_modes.clone(),
            default_output_modes: self.default_output_modes.clone(),
            skills: self.skills.clone(),
            provider: self.provider.clone(),
            security_schemes: Some(schemes),
            extensions: self.extensions.clone(),
        }
    }

    fn validate(&self, limits: &A2aLimits) -> A2aResult<()> {
        if self.capabilities.push_notifications {
            return Err(A2aError::ConfigInvalid {
                reason: "push notifications are not implemented by this facade".into(),
            });
        }
        // Use a valid probe URL to run the same wire validation as a decoded
        // card. The real URL is replaced with the bound address at start.
        let probe = A2aUrl::parse("http://127.0.0.1:1")?;
        let card = self.card_at(&probe);
        card.validate(limits)
    }
}

/// The facade is disabled unless a host explicitly opts in and supplies a
/// bearer token. A disabled policy never allocates a listener.
#[derive(Clone, Default)]
pub struct A2aPolicy {
    enabled: bool,
    listen: A2aListenAddr,
    bearer_token: Option<A2aSecretToken>,
    card: AgentCardSpec,
    limits: A2aLimits,
}

impl fmt::Debug for A2aPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("A2aPolicy")
            .field("enabled", &self.enabled)
            .field("listen", &self.listen)
            .field(
                "bearer_token",
                &if self.bearer_token.is_some() {
                    "<configured>"
                } else {
                    "<none>"
                },
            )
            .field("card_name_bytes", &self.card.name.len())
            .field("limits", &self.limits)
            .finish()
    }
}

impl A2aPolicy {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            card: AgentCardSpec::new(name, description, version),
            ..Self::default()
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn enable(mut self) -> Self {
        self.enabled = true;
        self
    }

    pub fn disable(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Explicit form of [`A2aPolicy::enable`] / [`A2aPolicy::disable`].
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Install the single bearer token. A second call replaces the first;
    /// the policy can never hold more than one token.
    pub fn with_bearer_token(mut self, token: impl Into<String>) -> A2aResult<Self> {
        self.bearer_token = Some(A2aSecretToken::new(token)?);
        Ok(self)
    }

    pub fn with_bearer_secret(mut self, token: A2aSecretToken) -> Self {
        self.bearer_token = Some(token);
        self
    }

    /// Alias for [`A2aPolicy::with_bearer_token`].
    pub fn with_token(self, token: impl Into<String>) -> A2aResult<Self> {
        self.with_bearer_token(token)
    }

    pub fn with_listen_addr(mut self, listen: A2aListenAddr) -> Self {
        self.listen = listen;
        self
    }

    /// Alias for [`A2aPolicy::with_listen_addr`].
    pub fn with_bind_addr(self, listen: A2aListenAddr) -> Self {
        self.with_listen_addr(listen)
    }

    /// Alias for [`A2aPolicy::with_listen`].
    pub fn with_bind(self, listen: impl AsRef<str>) -> A2aResult<Self> {
        self.with_listen(listen)
    }

    /// Parse and validate a literal loopback listen address. A public or
    /// hostname-based bind is rejected here and again by
    /// [`A2aPolicy::validate`] / [`crate::A2aServer::start`].
    pub fn with_listen(mut self, listen: impl AsRef<str>) -> A2aResult<Self> {
        self.listen = listen.as_ref().parse()?;
        Ok(self)
    }

    pub fn with_limits(mut self, limits: A2aLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_card_spec(mut self, card: AgentCardSpec) -> Self {
        self.card = card;
        self
    }

    pub fn with_skills(mut self, skills: Vec<AgentSkill>) -> Self {
        self.card.skills = skills;
        self
    }

    pub fn with_capabilities(mut self, capabilities: AgentCapabilities) -> Self {
        self.card.capabilities = capabilities;
        self
    }

    pub fn with_provider(mut self, provider: Option<AgentProvider>) -> Self {
        self.card.provider = provider;
        self
    }

    pub fn with_security_schemes(mut self, schemes: Option<Map<String, Value>>) -> Self {
        self.card.security_schemes = schemes;
        self
    }

    pub fn with_extensions(mut self, extensions: Option<Vec<AgentExtension>>) -> Self {
        self.card.extensions = extensions;
        self
    }

    pub fn with_default_modes(
        mut self,
        input_modes: Vec<String>,
        output_modes: Vec<String>,
    ) -> Self {
        self.card.default_input_modes = input_modes;
        self.card.default_output_modes = output_modes;
        self
    }

    pub fn listen_addr(&self) -> A2aListenAddr {
        self.listen
    }

    pub fn limits(&self) -> &A2aLimits {
        &self.limits
    }

    pub fn card_spec(&self) -> &AgentCardSpec {
        &self.card
    }

    pub fn bearer_token_configured(&self) -> bool {
        self.bearer_token.is_some()
    }

    pub(crate) fn take_token(&mut self) -> Option<A2aSecretToken> {
        self.bearer_token.take()
    }

    /// Validate everything that must be true before binding.
    pub fn validate(&self) -> A2aResult<()> {
        if !self.enabled {
            return Err(A2aError::PolicyDisabled);
        }
        if !is_exact_loopback(self.listen.socket_addr().ip()) {
            return Err(A2aError::LoopbackRequired {
                address: self.listen.to_string(),
            });
        }
        if self.bearer_token.is_none() {
            return Err(A2aError::ConfigInvalid {
                reason: "an enabled facade requires exactly one bearer token".into(),
            });
        }
        self.limits.validate()?;
        self.card.validate(&self.limits)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_policy() -> A2aPolicy {
        A2aPolicy::new("test", "test facade", "1.0.0")
            .with_bearer_token("test-token-0123456789")
            .expect("token")
            .enable()
    }

    #[test]
    fn defaults_to_disabled_and_requires_a_token() {
        let policy = A2aPolicy::default();
        assert!(!policy.is_enabled());
        assert_eq!(
            policy.validate().expect_err("disabled"),
            A2aError::PolicyDisabled
        );

        let policy = A2aPolicy::new("test", "test", "1").enable();
        assert_eq!(
            policy.validate().expect_err("missing token").code(),
            crate::error::A2aErrorCode::ConfigInvalid
        );
    }

    #[test]
    fn token_is_not_in_debug_output() {
        let policy = valid_policy();
        let debug = format!("{policy:?}");
        assert!(!debug.contains("test-token-0123456789"));
        assert!(debug.contains("<configured>"));
    }

    #[test]
    fn token_validation_is_strict_without_echoing_input() {
        assert!(A2aSecretToken::new("").is_err());
        assert!(A2aSecretToken::new("bad token").is_err());
        assert!(A2aSecretToken::new("line\nbreak").is_err());
        let error = A2aSecretToken::new("bad token").expect_err("whitespace");
        assert!(!error.to_string().contains("bad token"));
    }

    #[test]
    fn non_loopback_listen_is_rejected_by_the_policy_type() {
        let error = "0.0.0.0:0"
            .parse::<A2aListenAddr>()
            .expect_err("public bind");
        assert_eq!(error.code(), crate::error::A2aErrorCode::LoopbackRequired);
        // A policy can only hold an A2aListenAddr, so there is no way to get
        // a public listen address into the type without a validation error.
    }

    #[test]
    fn card_projection_uses_bound_loopback_and_bearer_scheme() {
        let policy = valid_policy();
        let url = A2aUrl::parse("http://127.0.0.1:4567").expect("url");
        let card = policy.card_spec().card_at(&url);
        assert_eq!(card.url, "http://127.0.0.1:4567");
        assert!(
            card.security_schemes
                .as_ref()
                .and_then(|schemes| schemes.get("bearerAuth"))
                .is_some()
        );
        card.validate(policy.limits()).expect("valid card");
    }

    #[test]
    fn push_notifications_are_not_advertised_as_supported() {
        let policy = valid_policy().with_capabilities(AgentCapabilities {
            streaming: true,
            push_notifications: true,
            state_transition_history: true,
        });
        assert_eq!(
            policy.validate().expect_err("push").code(),
            crate::error::A2aErrorCode::ConfigInvalid
        );
    }
}

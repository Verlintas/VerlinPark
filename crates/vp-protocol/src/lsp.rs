//! LSP/code-intelligence status DTOs.
//!
//! These are read-only diagnostics: they never start a language server and
//! never expose server command lines, environment, or file contents.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspStatusParams {
    pub project_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspLanguageStatus {
    pub language: String,
    #[serde(default)]
    pub display: String,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub install_hint: String,
    #[serde(default)]
    pub idle_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LspStatusResult {
    pub project_id: String,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub languages: Vec<LspLanguageStatus>,
    #[serde(default)]
    pub running: usize,
    #[serde(default)]
    pub installed: usize,
    #[serde(default)]
    pub supported_languages: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_round_trips_and_defaults_missing_fields() {
        let result = LspStatusResult {
            project_id: "proj-1".into(),
            root: "/tmp/project".into(),
            languages: vec![LspLanguageStatus {
                language: "rust".into(),
                display: "Rust".into(),
                server: Some("rust-analyzer".into()),
                installed: true,
                running: false,
                capabilities: vec!["workspace_symbols".into(), "references".into()],
                install_hint: "install rust-analyzer".into(),
                idle_secs: None,
            }],
            running: 0,
            installed: 1,
            supported_languages: vec!["rust".into(), "typescript".into()],
        };
        let value = serde_json::to_value(&result).unwrap();
        let decoded: LspStatusResult = serde_json::from_value(value).unwrap();
        assert_eq!(decoded, result);

        let minimal: LspStatusResult =
            serde_json::from_value(json!({"project_id": "proj-2"})).unwrap();
        assert_eq!(minimal.project_id, "proj-2");
        assert!(minimal.languages.is_empty());
        assert_eq!(minimal.running, 0);
        assert!(minimal.supported_languages.is_empty());
    }

    #[test]
    fn language_status_language_is_required_but_optional_fields_default() {
        let value = json!({"language": "python"});
        let decoded: LspLanguageStatus = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.language, "python");
        assert!(!decoded.installed);
        assert!(!decoded.running);
        assert!(decoded.server.is_none());
        assert!(decoded.capabilities.is_empty());
    }
}

use super::*;
use crate::permissions::PermissionAssessment;
use serde_json::{Value, json};
use time::OffsetDateTime;
use vp_protocol::types::{ApprovalGrant, ApprovalMode, ApprovalTemplate};

fn now() -> OffsetDateTime {
    OffsetDateTime::parse(
        "2026-11-12T12:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
}

fn assessment(risk: RiskLevel) -> PermissionAssessment {
    PermissionAssessment::new(risk, format!("test {}", risk.as_str()))
}

fn template(id: &str, tool: &str, action: &str) -> ApprovalTemplate {
    ApprovalTemplate {
        id: id.to_string(),
        project_id: "p1".to_string(),
        name: format!("template {id}"),
        enabled: true,
        tool_pattern: tool.to_string(),
        risk_levels: vec!["external".to_string(), "workspace_write".to_string()],
        path_globs: Vec::new(),
        action: action.to_string(),
        expires_at: None,
        max_uses: None,
        used_count: 0,
        created_by: "test".to_string(),
        created_at: "2026-11-12T00:00:00Z".to_string(),
        updated_at: None,
    }
}

fn grant(tool: &str, path: &str, children: bool) -> ApprovalGrant {
    ApprovalGrant {
        id: "g1".to_string(),
        project_id: "p1".to_string(),
        tool: tool.to_string(),
        path_prefix: path.to_string(),
        include_children: children,
        created_by: "test".to_string(),
        created_at: "2026-11-12T00:00:00Z".to_string(),
    }
}

fn evaluate(
    templates: &[ApprovalTemplate],
    grants: &[ApprovalGrant],
    tool: &str,
    input: &Value,
    risk: RiskLevel,
    mode: ApprovalMode,
    auto_approve: bool,
) -> ApprovalEvaluation {
    let assessment = assessment(risk);
    let mut consume = |template: &ApprovalTemplate| Some(template.remaining_uses());
    evaluate_approval_rules(
        templates,
        grants,
        &assessment,
        tool,
        input,
        mode,
        auto_approve,
        now(),
        &mut consume,
    )
}

#[test]
fn explicit_deny_wins_over_every_template_and_grant() {
    let blocked = PermissionAssessment::blocked("privilege escalation");
    let mut consume = |_template: &ApprovalTemplate| Some(Some(1));
    let evaluation = evaluate_approval_rules(
        &[
            template("allow", "shell", "allow"),
            template("deny", "shell", "deny"),
        ],
        &[grant("shell", "src", true)],
        &blocked,
        "shell",
        &json!({"command": "sudo rm -rf /", "path": "src/lib.rs"}),
        ApprovalMode::Smart,
        false,
        now(),
        &mut consume,
    );
    assert!(matches!(evaluation.decision, ApprovalDecision::Deny { .. }));
    assert_eq!(evaluation.source, ApprovalDecisionSource::ExplicitDeny);
}

#[test]
fn template_deny_beats_grant_and_allow() {
    let mut deny = template("deny", "write_*", "deny");
    deny.path_globs = vec!["secrets/**".to_string()];
    let evaluation = evaluate(
        &[template("allow", "write_*", "allow"), deny],
        &[grant("write_file", "secrets", true)],
        "write_file",
        &json!({"path": "secrets/key.txt"}),
        RiskLevel::WorkspaceWrite,
        ApprovalMode::Smart,
        false,
    );
    assert!(matches!(evaluation.decision, ApprovalDecision::Deny { .. }));
    assert_eq!(evaluation.source, ApprovalDecisionSource::TemplateDeny);
    assert_eq!(evaluation.template_id.as_deref(), Some("deny"));
    assert_eq!(evaluation.scope, vec!["secrets/**".to_string()]);
}

#[test]
fn exact_grant_beats_template_allow() {
    let evaluation = evaluate(
        &[template("allow", "write_file", "allow")],
        &[grant("write_file", "src", true)],
        "write_file",
        &json!({"path": "src/lib.rs"}),
        RiskLevel::WorkspaceWrite,
        ApprovalMode::Strict,
        false,
    );
    assert!(matches!(
        evaluation.decision,
        ApprovalDecision::Allow { .. }
    ));
    assert_eq!(evaluation.source, ApprovalDecisionSource::Grant);
}

#[test]
fn template_allow_beats_risk_policy_ask() {
    let evaluation = evaluate(
        &[template("allow", "write_file", "allow")],
        &[],
        "write_file",
        &json!({"path": "src/lib.rs"}),
        RiskLevel::WorkspaceWrite,
        ApprovalMode::Strict,
        false,
    );
    assert!(matches!(
        evaluation.decision,
        ApprovalDecision::Allow { .. }
    ));
    assert_eq!(evaluation.source, ApprovalDecisionSource::TemplateAllow);
}

#[test]
fn escalate_template_forces_a_human_without_approving() {
    let evaluation = evaluate(
        &[template("escalate", "run_project_task", "escalate")],
        &[],
        "run_project_task",
        &json!({"kind": "test"}),
        RiskLevel::WorkspaceWrite,
        ApprovalMode::Smart,
        false,
    );
    assert!(matches!(evaluation.decision, ApprovalDecision::Ask { .. }));
    assert_eq!(evaluation.source, ApprovalDecisionSource::TemplateEscalate);
}

#[test]
fn risk_policy_remains_the_fallback() {
    let evaluation = evaluate(
        &[],
        &[],
        "computer",
        &json!({"action": "click"}),
        RiskLevel::HostState,
        ApprovalMode::Smart,
        false,
    );
    assert!(matches!(evaluation.decision, ApprovalDecision::Ask { .. }));
    assert_eq!(evaluation.source, ApprovalDecisionSource::RiskPolicy);
}

#[test]
fn allow_templates_never_cover_destructive_or_host_state() {
    for risk in [RiskLevel::HostState, RiskLevel::Destructive] {
        let mut configured = template("bad", "computer", "allow");
        configured.risk_levels = vec![risk.as_str().to_string()];
        assert!(!template_can_allow(&configured, "computer", risk));
        let evaluation = evaluate(
            &[configured],
            &[],
            "computer",
            &json!({"action": "type"}),
            risk,
            ApprovalMode::Smart,
            false,
        );
        assert!(matches!(evaluation.decision, ApprovalDecision::Ask { .. }));
        assert_eq!(evaluation.source, ApprovalDecisionSource::RiskPolicy);
    }
}

#[test]
fn allow_templates_never_cover_remote_tools() {
    let mut remote = template("remote", "remote_exec", "allow");
    remote.risk_levels = vec!["external".to_string()];
    let evaluation = evaluate(
        &[remote],
        &[],
        "remote_exec",
        &json!({"command": "ls"}),
        RiskLevel::External,
        ApprovalMode::Strict,
        false,
    );
    assert!(matches!(evaluation.decision, ApprovalDecision::Ask { .. }));
    assert_eq!(evaluation.source, ApprovalDecisionSource::RiskPolicy);
}

#[test]
fn allow_template_rejects_forbidden_risk_levels_at_validation() {
    let error = validate_risk_levels(&["destructive".to_string()], ApprovalTemplateAction::Allow)
        .unwrap_err();
    assert_eq!(error.code, APPROVAL_TEMPLATE_FORBIDDEN_RISK);
    let deny =
        validate_risk_levels(&["destructive".to_string()], ApprovalTemplateAction::Deny).unwrap();
    assert_eq!(deny, vec!["destructive".to_string()]);
}

#[test]
fn unknown_risk_names_fail_closed() {
    let mut unknown = template("unknown", "write_file", "allow");
    unknown.risk_levels = vec!["future_risk".to_string()];
    let evaluation = evaluate(
        &[unknown],
        &[],
        "write_file",
        &json!({"path": "src/lib.rs"}),
        RiskLevel::WorkspaceWrite,
        ApprovalMode::Smart,
        false,
    );
    assert!(matches!(
        evaluation.decision,
        ApprovalDecision::Allow { .. }
    ));
    assert_eq!(evaluation.source, ApprovalDecisionSource::RiskPolicy);
    let error = validate_risk_levels(&["future_risk".to_string()], ApprovalTemplateAction::Deny)
        .unwrap_err();
    assert_eq!(error.code, APPROVAL_TEMPLATE_INVALID);
}

#[test]
fn exhausted_template_is_inert_and_next_candidate_can_apply() {
    let mut exhausted = template("exhausted", "write_file", "allow");
    exhausted.max_uses = Some(2);
    exhausted.used_count = 2;
    assert!(!template_is_live(&exhausted, now()));
    let assessment = assessment(RiskLevel::WorkspaceWrite);
    let mut attempts = Vec::new();
    let mut consume = |candidate: &ApprovalTemplate| {
        attempts.push(candidate.id.clone());
        if candidate.id == "exhausted" {
            None
        } else {
            Some(Some(4))
        }
    };
    let evaluation = evaluate_approval_rules(
        &[exhausted, template("wide", "write_*", "allow")],
        &[],
        &assessment,
        "write_file",
        &json!({"path": "src/lib.rs"}),
        ApprovalMode::Strict,
        false,
        now(),
        &mut consume,
    );
    assert!(matches!(
        evaluation.decision,
        ApprovalDecision::Allow { .. }
    ));
    assert_eq!(evaluation.template_id.as_deref(), Some("wide"));
    assert_eq!(evaluation.remaining_uses, Some(4));
    assert_eq!(attempts, vec!["wide".to_string()]);
}

#[test]
fn expired_and_malformed_expiry_are_inert() {
    let mut expired = template("expired", "write_file", "allow");
    expired.expires_at = Some("2026-11-11T00:00:00Z".to_string());
    assert!(!template_is_live(&expired, now()));
    let mut malformed = template("malformed", "write_file", "allow");
    malformed.expires_at = Some("tomorrow".to_string());
    assert!(!template_is_live(&malformed, now()));
    let mut future = template("future", "write_file", "allow");
    future.expires_at = Some("2026-11-13T00:00:00Z".to_string());
    assert!(template_is_live(&future, now()));

    let mut poisoned = template("poisoned", "*", "allow");
    poisoned.path_globs = vec!["../secrets/**".to_string()];
    assert!(!template_is_live(&poisoned, now()));
    poisoned.path_globs = Vec::new();
    poisoned.tool_pattern = "*".to_string();
    assert!(!template_is_live(&poisoned, now()));
}

#[test]
fn use_caps_are_reported_as_remaining_and_zero_is_exhausted() {
    let mut capped = template("capped", "write_file", "allow");
    capped.max_uses = Some(3);
    capped.used_count = 1;
    let assessment = assessment(RiskLevel::WorkspaceWrite);
    let mut consume = |_candidate: &ApprovalTemplate| Some(Some(0));
    let evaluation = evaluate_approval_rules(
        &[capped],
        &[],
        &assessment,
        "write_file",
        &json!({"path": "src/lib.rs"}),
        ApprovalMode::Strict,
        false,
        now(),
        &mut consume,
    );
    assert_eq!(evaluation.remaining_uses, Some(0));
    let mut zero = template("zero", "write_file", "allow");
    zero.max_uses = Some(0);
    assert!(!template_is_live(&zero, now()));
}

#[test]
fn tool_patterns_are_anchored_and_bounded() {
    assert!(tool_pattern_matches("write_file", "write_file"));
    assert!(!tool_pattern_matches("write_file", "write_files"));
    assert!(tool_pattern_matches("write_*", "write_file"));
    assert!(!tool_pattern_matches("write_*", "read_file"));
    assert!(tool_pattern_matches("write_?_file", "write_a_file"));
    assert!(!tool_pattern_matches("write_?_file", "write_ab_file"));
    assert!(tool_pattern_matches("*_file", "write_file"));
    assert!(!tool_pattern_matches("*_file", "write_file_x"));

    assert!(validate_tool_pattern("write_*").is_ok());
    assert!(validate_tool_pattern("write_file").is_ok());
    for bad in ["", "*", "?", "*a*", "a**", "write file", "a\nb"] {
        assert!(validate_tool_pattern(bad).is_err(), "{bad}");
    }
}

#[test]
fn path_globs_do_not_cross_separators_without_double_star() {
    assert!(path_glob_matches("src/*.rs", "src/lib.rs"));
    assert!(!path_glob_matches("src/*.rs", "src/nested/lib.rs"));
    assert!(path_glob_matches("src/**/*.rs", "src/nested/lib.rs"));
    assert!(path_glob_matches("src/**/*.rs", "src/lib.rs"));
    assert!(path_glob_matches("src/**", "src/nested/lib.rs"));
    assert!(path_glob_matches("./src/lib.rs", "src/lib.rs"));
    assert!(!path_glob_matches("src/lib.rs", "src/lib.rsx"));
    assert!(path_glob_matches("src/?.rs", "src/a.rs"));
    assert!(!path_glob_matches("src/?.rs", "src/ab.rs"));
}

#[test]
fn path_glob_validation_rejects_escapes_and_absolute_paths() {
    assert!(validate_path_globs(&["src/**".to_string()]).is_ok());
    for bad in [
        "/etc/passwd",
        "~/secrets",
        "../outside",
        "src/../outside",
        "src//lib.rs",
        "C:/secret",
    ] {
        assert!(validate_path_globs(&[bad.to_string()]).is_err(), "{bad}");
    }
    let many = (0..=MAX_TEMPLATE_PATH_GLOBS)
        .map(|index| format!("src/{index}.rs"))
        .collect::<Vec<_>>();
    assert!(validate_path_globs(&many).is_err());
}

#[test]
fn template_paths_require_a_matching_path() {
    let mut scoped = template("scoped", "write_file", "allow");
    scoped.path_globs = vec!["src/**".to_string()];
    let assessment = assessment(RiskLevel::WorkspaceWrite);
    let mut consume = |_candidate: &ApprovalTemplate| Some(None);
    let missing = evaluate_approval_rules(
        &[scoped.clone()],
        &[],
        &assessment,
        "run_project_task",
        &json!({"kind": "test"}),
        ApprovalMode::Smart,
        false,
        now(),
        &mut consume,
    );
    assert_eq!(missing.source, ApprovalDecisionSource::RiskPolicy);
    let matching = evaluate_approval_rules(
        &[scoped],
        &[],
        &assessment,
        "write_file",
        &json!({"path": "src/lib.rs"}),
        ApprovalMode::Smart,
        false,
        now(),
        &mut consume,
    );
    assert_eq!(matching.source, ApprovalDecisionSource::TemplateAllow);
}

#[test]
fn auto_approve_honors_deny_but_not_allow_templates() {
    let allow = evaluate(
        &[template("allow", "remote_write", "allow")],
        &[],
        "shell",
        &json!({"command": "ls"}),
        RiskLevel::External,
        ApprovalMode::Smart,
        true,
    );
    assert!(matches!(allow.decision, ApprovalDecision::Allow { .. }));
    assert_eq!(allow.source, ApprovalDecisionSource::RiskPolicy);

    let deny = evaluate(
        &[template("deny", "shell", "deny")],
        &[],
        "shell",
        &json!({"command": "ls"}),
        RiskLevel::WorkspaceWrite,
        ApprovalMode::Smart,
        true,
    );
    assert!(matches!(deny.decision, ApprovalDecision::Deny { .. }));
    assert_eq!(deny.source, ApprovalDecisionSource::TemplateDeny);
}

#[test]
fn canonical_json_is_order_independent_and_scope_hash_is_stable() {
    let left = json!({"b": 2, "a": {"y": 1, "x": 0}});
    let right = json!({"a": {"x": 0, "y": 1}, "b": 2});
    assert_eq!(canonical_json(&left), canonical_json(&right));
    assert_eq!(approval_scope_hash(&left), approval_scope_hash(&right));
    let other = json!({"a": {"x": 0, "y": 2}, "b": 2});
    assert_ne!(approval_scope_hash(&left), approval_scope_hash(&other));
}

#[test]
fn dedup_key_includes_session_agent_tool_scope_and_risk() {
    let scope = approval_scope_hash(&json!({"path": "src/lib.rs"}));
    let agent = json!({"agent_id": "worker-1"});
    let base = approval_dedup_key("s1", Some(&agent), "write_file", &scope, "workspace_write");
    assert_eq!(
        base,
        approval_dedup_key("s1", Some(&agent), "write_file", &scope, "workspace_write")
    );
    assert_ne!(
        base,
        approval_dedup_key("s2", Some(&agent), "write_file", &scope, "workspace_write")
    );
    assert_ne!(
        base,
        approval_dedup_key(
            "s1",
            Some(&json!({"agent_id": "worker-2"})),
            "write_file",
            &scope,
            "workspace_write"
        )
    );
    assert_ne!(
        base,
        approval_dedup_key("s1", Some(&agent), "edit_file", &scope, "workspace_write")
    );
    assert_ne!(
        base,
        approval_dedup_key("s1", Some(&agent), "write_file", &scope, "external")
    );
}

#[test]
fn grant_matching_requires_known_paths_and_exact_child_prefixes() {
    let grants = vec![grant("write_file", "src", true)];
    assert!(match_grant(&grants, "write_file", &["src/lib.rs".to_string()]).is_some());
    assert!(match_grant(&grants, "write_file", &["src".to_string()]).is_some());
    assert!(match_grant(&grants, "write_file", &["source/lib.rs".to_string()]).is_none());
    assert!(match_grant(&grants, "edit_file", &["src/lib.rs".to_string()]).is_none());
    assert!(match_grant(&grants, "write_file", &[]).is_none());
}

#[test]
fn template_from_approval_refuses_high_risk_remote_and_opaque_tools() {
    let base = Approval {
        id: "a1".into(),
        session_id: "s1".into(),
        tool_call_id: "c1".into(),
        tool: "write_file".into(),
        input: json!({"path": "src/lib.rs"}),
        status: "approved".into(),
        created_at: "2026-11-12T00:00:00Z".into(),
        resolved_at: Some("2026-11-12T00:01:00Z".into()),
        approval_risk: Some("workspace_write".into()),
        approval_reason: None,
        agent_id: None,
        agent_name: None,
        agent_role: None,
        agent_title: None,
        task_id: None,
        scope_hash: None,
        dedup_key: None,
        waiter_count: 1,
        template_id: None,
        priority: 0,
        expires_at: None,
        notify: true,
        notify_key: None,
    };
    let template = template_from_approval(&base, None, Some(3), None, "operator", now()).unwrap();
    assert_eq!(template.tool_pattern, "write_file");
    assert_eq!(template.path_globs, vec!["src/lib.rs".to_string()]);
    assert_eq!(template.max_uses, Some(3));
    assert_eq!(template.risk_levels, vec!["workspace_write".to_string()]);

    for (tool, risk) in [
        ("computer", "host_state"),
        ("remote_write", "external"),
        ("write_file", "destructive"),
        ("shell", "workspace_write"),
    ] {
        let mut approval = base.clone();
        approval.tool = tool.to_string();
        approval.approval_risk = Some(risk.to_string());
        assert!(template_from_approval(&approval, None, None, None, "operator", now()).is_err());
    }
}

#[test]
fn template_expiry_validation_is_bounded() {
    assert!(validate_template_expiry(None, now()).unwrap().is_none());
    assert!(validate_template_expiry(Some("2026-11-13T00:00:00Z"), now()).is_ok());
    assert!(validate_template_expiry(Some("2026-11-11T00:00:00Z"), now()).is_err());
    assert!(validate_template_expiry(Some("tomorrow"), now()).is_err());
    assert!(validate_template_expiry(Some("2028-01-01T00:00:00Z"), now()).is_err());
}

#[test]
fn expiry_boosts_high_risk_but_remains_finite() {
    let policy = AttentionPolicy {
        stale_after_secs: 60,
        priority_boost_risk: "host_state".into(),
        ..AttentionPolicy::default()
    };
    let normal = approval_expires_at(now(), Some("external"), &policy);
    let boosted = approval_expires_at(now(), Some("host_state"), &policy);
    let normal =
        OffsetDateTime::parse(&normal, &time::format_description::well_known::Rfc3339).unwrap();
    let boosted =
        OffsetDateTime::parse(&boosted, &time::format_description::well_known::Rfc3339).unwrap();
    assert_eq!((normal - now()).whole_seconds(), 60);
    assert_eq!((boosted - now()).whole_seconds(), 240);
}

#[test]
fn attention_policy_normalization_is_idempotent() {
    let policy = AttentionPolicy {
        enabled: true,
        max_pending_per_session: 0,
        coalesce_window_secs: 9_999,
        stale_after_secs: 1,
        priority_boost_risk: "bogus".into(),
        notify_coalesce_secs: 9_999,
    };
    let once = validate_attention_policy(policy.clone());
    let twice = validate_attention_policy(once.clone());
    assert_eq!(once, twice);
    assert_eq!(once.max_pending_per_session, 1);
    assert_eq!(once.stale_after_secs, 60);
}

#[test]
fn error_data_has_the_q3_shape() {
    let error = ApprovalRuleError::forbidden_risk("host state");
    let data = error.data();
    assert_eq!(data["code"], APPROVAL_TEMPLATE_FORBIDDEN_RISK);
    assert_eq!(data["retryable"], false);
    assert!(!data["hint"].as_str().unwrap().is_empty());
    let (_, retryable) = error_hint(APPROVAL_NOT_FOUND);
    assert!(!retryable);
}

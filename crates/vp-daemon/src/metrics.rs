use std::fmt::Write as _;

use vp_protocol::types::{DaemonMetrics, MetricCount};

/// Render a `DaemonMetrics` snapshot as Prometheus text exposition format
/// (version 0.0.4). The output contains only fixed metric names and bounded
/// status/kind labels; it never includes prompts, paths, credentials, model
/// names, agent ids, or session ids.
pub fn render_prometheus(metrics: &DaemonMetrics) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# HELP verlinpark_up 1 if the VerlinPark daemon is running."
    );
    let _ = writeln!(out, "# TYPE verlinpark_up gauge");
    let _ = writeln!(out, "verlinpark_up 1");
    let _ = writeln!(
        out,
        "# HELP verlinpark_info Build and protocol information."
    );
    let _ = writeln!(out, "# TYPE verlinpark_info gauge");
    let _ = writeln!(
        out,
        "verlinpark_info{{version=\"{}\",protocol=\"{}\"}} 1",
        escape_label(&metrics.version),
        metrics.protocol
    );
    let _ = writeln!(
        out,
        "# HELP verlinpark_uptime_seconds Seconds since the daemon started."
    );
    let _ = writeln!(out, "# TYPE verlinpark_uptime_seconds gauge");
    let _ = writeln!(out, "verlinpark_uptime_seconds {}", metrics.uptime_secs);
    let _ = writeln!(out, "# HELP verlinpark_projects_total Registered projects.");
    let _ = writeln!(out, "# TYPE verlinpark_projects_total gauge");
    let _ = writeln!(out, "verlinpark_projects_total {}", metrics.projects);
    render_group(
        &mut out,
        "verlinpark_sessions",
        "Sessions by status.",
        "status",
        &metrics.sessions,
    );
    render_group(
        &mut out,
        "verlinpark_plan_tasks",
        "Persisted plan tasks by status.",
        "status",
        &metrics.plan_tasks,
    );
    render_group(
        &mut out,
        "verlinpark_merge_queue",
        "Merge-queue entries by status.",
        "status",
        &metrics.merge_queue,
    );
    render_group(
        &mut out,
        "verlinpark_intake",
        "Standalone task-inbox items by status.",
        "status",
        &metrics.intake,
    );
    render_group(
        &mut out,
        "verlinpark_continuations",
        "Deferred session continuations by status.",
        "status",
        &metrics.continuations,
    );
    render_group(
        &mut out,
        "verlinpark_recoveries",
        "Stall-recovery records by status.",
        "status",
        &metrics.recoveries,
    );
    render_group(
        &mut out,
        "verlinpark_memories",
        "Project memories by status.",
        "status",
        &metrics.memories,
    );
    render_group(
        &mut out,
        "verlinpark_memory_conflicts",
        "Memory conflicts by status.",
        "status",
        &metrics.memory_conflicts,
    );
    render_group(
        &mut out,
        "verlinpark_runtime_leases",
        "Runtime leases by kind.",
        "kind",
        &metrics.runtime_leases,
    );
    render_group(
        &mut out,
        "verlinpark_supervised_processes",
        "Durable supervised processes by state.",
        "state",
        &metrics.supervised_processes,
    );
    let _ = writeln!(
        out,
        "# HELP verlinpark_supervised_process_output_bytes Retained bytes across supervised-process logs."
    );
    let _ = writeln!(
        out,
        "# TYPE verlinpark_supervised_process_output_bytes gauge"
    );
    let _ = writeln!(
        out,
        "verlinpark_supervised_process_output_bytes {}",
        metrics.supervised_process_output_bytes
    );
    let _ = writeln!(
        out,
        "# HELP verlinpark_tokens_total Provider tokens by direction."
    );
    let _ = writeln!(out, "# TYPE verlinpark_tokens_total counter");
    let _ = writeln!(
        out,
        "verlinpark_tokens_total{{direction=\"in\"}} {}",
        metrics.tokens_in
    );
    let _ = writeln!(
        out,
        "verlinpark_tokens_total{{direction=\"out\"}} {}",
        metrics.tokens_out
    );
    let _ = writeln!(
        out,
        "# HELP verlinpark_cache_tokens_total Provider prompt-cache tokens by direction."
    );
    let _ = writeln!(out, "# TYPE verlinpark_cache_tokens_total counter");
    let _ = writeln!(
        out,
        "verlinpark_cache_tokens_total{{direction=\"read\"}} {}",
        metrics.cache_read_tokens
    );
    let _ = writeln!(
        out,
        "verlinpark_cache_tokens_total{{direction=\"write\"}} {}",
        metrics.cache_write_tokens
    );
    let _ = writeln!(
        out,
        "# HELP verlinpark_cache_hit_ratio Cache-read share over ledger input buckets (0..1)."
    );
    let _ = writeln!(out, "# TYPE verlinpark_cache_hit_ratio gauge");
    let _ = writeln!(
        out,
        "verlinpark_cache_hit_ratio {}",
        format_ratio(metrics.cache_hit_ratio)
    );
    if let Some(tokens) = metrics.preamble_tokens_estimate {
        let _ = writeln!(
            out,
            "# HELP verlinpark_preamble_tokens_estimate Estimated stable system + tool-schema preamble tokens of the latest agent run."
        );
        let _ = writeln!(out, "# TYPE verlinpark_preamble_tokens_estimate gauge");
        let _ = writeln!(out, "verlinpark_preamble_tokens_estimate {tokens}");
    }
    let _ = writeln!(
        out,
        "# HELP verlinpark_cost_usd_total Recorded provider spend in USD."
    );
    let _ = writeln!(out, "# TYPE verlinpark_cost_usd_total counter");
    let _ = writeln!(
        out,
        "verlinpark_cost_usd_total {}",
        format_non_negative(metrics.cost_spent_usd)
    );
    out
}

fn render_group(out: &mut String, name: &str, help: &str, label: &str, groups: &[MetricCount]) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let mut ordered = groups.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.key.cmp(&right.key));
    for group in ordered {
        let _ = writeln!(
            out,
            "{name}{{{label}=\"{}\"}} {}",
            escape_label(&group.key),
            group.count
        );
    }
}

/// Escape a Prometheus label value per the text exposition format. Carriage
/// returns are replaced because the format has no `\r` escape and a raw one
/// would break line-oriented parsers.
fn escape_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push(' '),
            _ => escaped.push(character),
        }
    }
    escaped
}

fn format_non_negative(value: f64) -> String {
    if value.is_finite() {
        format!("{}", value.max(0.0))
    } else {
        "0".to_string()
    }
}

fn format_ratio(value: f64) -> String {
    if value.is_finite() {
        format!("{}", value.clamp(0.0, 1.0))
    } else {
        "0".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(key: &str, value: u64) -> MetricCount {
        MetricCount {
            key: key.to_string(),
            count: value,
        }
    }

    fn sample_metrics() -> DaemonMetrics {
        DaemonMetrics {
            version: "0.1.0-dev".to_string(),
            protocol: 1,
            uptime_secs: 12,
            projects: 2,
            sessions: vec![count("running", 2), count("draft", 1)],
            plan_tasks: vec![count("pending", 3)],
            merge_queue: vec![count("merged", 1)],
            intake: vec![count("queued", 4)],
            continuations: vec![count("pending", 1)],
            recoveries: vec![count("retry_requested", 1)],
            memories: vec![count("active", 5)],
            memory_conflicts: vec![count("open", 1)],
            runtime_leases: vec![count("session", 2)],
            supervised_processes: vec![count("running", 1), count("lost", 1)],
            supervised_process_output_bytes: 512,
            tokens_in: 1000,
            tokens_out: 250,
            cache_read_tokens: 400,
            cache_write_tokens: 100,
            cache_hit_ratio: 0.4,
            preamble_tokens_estimate: Some(7000),
            cost_spent_usd: 0.125,
        }
    }

    #[test]
    fn renders_all_metric_names_with_stable_ordering() {
        let metrics = sample_metrics();
        let rendered = render_prometheus(&metrics);

        for name in [
            "verlinpark_up",
            "verlinpark_info",
            "verlinpark_uptime_seconds",
            "verlinpark_projects_total",
            "verlinpark_sessions",
            "verlinpark_plan_tasks",
            "verlinpark_merge_queue",
            "verlinpark_intake",
            "verlinpark_continuations",
            "verlinpark_recoveries",
            "verlinpark_memories",
            "verlinpark_memory_conflicts",
            "verlinpark_runtime_leases",
            "verlinpark_supervised_processes",
            "verlinpark_supervised_process_output_bytes",
            "verlinpark_tokens_total",
            "verlinpark_cache_tokens_total",
            "verlinpark_cache_hit_ratio",
            "verlinpark_preamble_tokens_estimate",
            "verlinpark_cost_usd_total",
        ] {
            assert!(
                rendered.contains(&format!("# HELP {name} ")),
                "missing HELP for {name}"
            );
            assert!(
                rendered.contains(&format!("# TYPE {name} ")),
                "missing TYPE for {name}"
            );
        }

        assert!(rendered.contains("verlinpark_up 1\n"));
        assert!(rendered.contains("verlinpark_info{version=\"0.1.0-dev\",protocol=\"1\"} 1\n"));
        assert!(rendered.contains("verlinpark_uptime_seconds 12\n"));
        assert!(rendered.contains("verlinpark_projects_total 2\n"));
        assert!(rendered.contains("verlinpark_sessions{status=\"draft\"} 1\n"));
        assert!(rendered.contains("verlinpark_sessions{status=\"running\"} 2\n"));
        assert!(rendered.contains("verlinpark_runtime_leases{kind=\"session\"} 2\n"));
        assert!(rendered.contains("verlinpark_supervised_processes{state=\"running\"} 1\n"));
        assert!(rendered.contains("verlinpark_supervised_processes{state=\"lost\"} 1\n"));
        assert!(rendered.contains("verlinpark_supervised_process_output_bytes 512\n"));
        assert!(rendered.contains("verlinpark_tokens_total{direction=\"in\"} 1000\n"));
        assert!(rendered.contains("verlinpark_tokens_total{direction=\"out\"} 250\n"));
        assert!(rendered.contains("verlinpark_cache_tokens_total{direction=\"read\"} 400\n"));
        assert!(rendered.contains("verlinpark_cache_tokens_total{direction=\"write\"} 100\n"));
        assert!(rendered.contains("verlinpark_cache_hit_ratio 0.4\n"));
        assert!(rendered.contains("verlinpark_preamble_tokens_estimate 7000\n"));
        assert!(rendered.contains("verlinpark_cost_usd_total 0.125\n"));
        assert!(rendered.ends_with('\n'));

        let draft = rendered
            .find("verlinpark_sessions{status=\"draft\"}")
            .unwrap();
        let running = rendered
            .find("verlinpark_sessions{status=\"running\"}")
            .unwrap();
        assert!(draft < running, "sessions must be ordered by key");
        assert_eq!(rendered, render_prometheus(&sample_metrics()));
    }

    #[test]
    fn escapes_label_values_and_omits_unbounded_data() {
        let mut metrics = sample_metrics();
        metrics.version = "1.0\"beta\\night".to_string();
        metrics.sessions = vec![count("run\nning\"x\\y", 3)];
        metrics.intake = Vec::new();

        let rendered = render_prometheus(&metrics);
        assert!(rendered.contains("version=\"1.0\\\"beta\\\\night\""));
        assert!(
            rendered.contains("verlinpark_sessions{status=\"run\\nning\\\"x\\\\y\"} 3"),
            "unexpected escaped sample: {rendered}"
        );
        assert!(!rendered.contains("/Users/"));
        assert!(!rendered.contains("sk-secret"));
        assert!(!rendered.contains("session_id"));
        assert!(!rendered.contains("task_id"));
        // Empty categories still advertise HELP/TYPE but emit no series.
        assert!(rendered.contains("# TYPE verlinpark_intake gauge"));
        assert!(!rendered.contains("verlinpark_intake{"));
    }

    #[test]
    fn clamps_non_finite_and_negative_cost() {
        let mut metrics = sample_metrics();
        metrics.cost_spent_usd = f64::NAN;
        assert!(
            render_prometheus(&metrics).contains("verlinpark_cost_usd_total 0\n"),
            "NaN cost must not be rendered"
        );
        metrics.cost_spent_usd = -1.5;
        assert!(
            render_prometheus(&metrics).contains("verlinpark_cost_usd_total 0\n"),
            "negative cost must be clamped"
        );
    }

    #[test]
    fn clamps_ratio_and_omits_unavailable_preamble() {
        let mut metrics = sample_metrics();
        metrics.cache_hit_ratio = 7.5;
        metrics.preamble_tokens_estimate = None;

        let rendered = render_prometheus(&metrics);
        assert!(rendered.contains("verlinpark_cache_hit_ratio 1\n"));
        assert!(
            !rendered.contains("verlinpark_preamble_tokens_estimate"),
            "unavailable preamble must be omitted entirely: {rendered}"
        );

        metrics.cache_hit_ratio = f64::NAN;
        assert!(render_prometheus(&metrics).contains("verlinpark_cache_hit_ratio 0\n"));
    }
}

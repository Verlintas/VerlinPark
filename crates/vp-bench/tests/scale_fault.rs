//! Deterministic scale/fault harness tests.
//!
//! These tests use disposable repositories/SQLite stores and an in-process fake
//! provider. They perform no network I/O and require no provider key.

use std::collections::BTreeSet;

use vp_bench::cli::{BenchCommand, ScaleArgs, execute};
use vp_bench::scale_fault::{
    FaultBoundary, SCALE_FAULT_SCHEMA, SCALE_TASK_COUNT, run_fault_report, run_scale_report,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scale_report_validates_and_completes_100_tasks() {
    let report = run_scale_report(10).await.unwrap();
    assert!(
        report.passed,
        "failed checks: {:?}",
        report
            .checks
            .iter()
            .filter(|check| !check.passed)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.schema, SCALE_FAULT_SCHEMA);
    assert_eq!(report.task_count, SCALE_TASK_COUNT);
    assert_eq!(report.completed_tasks, SCALE_TASK_COUNT);
    assert_eq!(report.max_workers, 10);
    assert_eq!(report.dependency_layers, 10);
    assert_eq!(report.batch_count, 10);
    assert_eq!(report.peak_batch, 10);
    assert_eq!(report.checks_failed, 0);
    assert!(report.checks_total >= 12, "{:?}", report.checks);

    for name in [
        "plan_validation_accepts_100_tasks",
        "plan_validation_rejects_101_tasks",
        "plan_validation_rejects_duplicate_ids",
        "plan_validation_rejects_cycles",
        "queue_projection_has_all_100_tasks",
        "dependency_graph_is_acyclic",
        "no_task_lost_or_dispatched_twice",
        "no_overlapping_write_scope_in_a_batch",
    ] {
        assert!(
            report.checks.iter().any(|check| check.name == name),
            "missing check {name}"
        );
    }

    let dispatched = report
        .batches
        .iter()
        .flat_map(|batch| batch.task_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(dispatched.len(), SCALE_TASK_COUNT);
    assert!(
        report
            .batches
            .iter()
            .all(|batch| batch.task_ids.len() <= 10 && !batch.task_ids.is_empty())
    );
    assert!(
        report
            .caveats
            .iter()
            .any(|caveat| caveat.contains("synthetic claim/finish"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scale_report_supports_a_lower_worker_cap_without_overlap() {
    let report = run_scale_report(4).await.unwrap();
    assert!(
        report.passed,
        "failed checks: {:?}",
        report
            .checks
            .iter()
            .filter(|check| !check.passed)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.max_workers, 4);
    assert_eq!(report.peak_batch, 4);
    assert_eq!(report.batch_count, 30);
    assert_eq!(report.completed_tasks, SCALE_TASK_COUNT);
    assert!(report.batches.iter().all(|batch| batch.task_ids.len() <= 4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fault_report_covers_the_matrix_with_honest_boundaries() {
    let report = run_fault_report().await.unwrap();
    assert!(
        report.passed,
        "failed cases: {:?}",
        report
            .cases
            .iter()
            .filter(|case| !case.passed)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.case_count, 5);
    assert_eq!(report.cases_passed, 5);
    assert_eq!(report.cases_failed, 0);
    assert_eq!(report.checks_failed, 0);
    let ids = report
        .cases
        .iter()
        .map(|case| case.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        ids,
        BTreeSet::from([
            "abort-boundary-cancellation",
            "duplicate-tool-call-replay-guard",
            "lost-task-and-batch-abort",
            "provider-output-truncation",
            "provider-stream-failure-before-output",
        ])
    );
    for case in &report.cases {
        match case.id.as_str() {
            "provider-stream-failure-before-output"
            | "provider-output-truncation"
            | "abort-boundary-cancellation" => {
                assert_eq!(case.boundary, FaultBoundary::RealRuntimeProvider);
            }
            _ => assert_eq!(case.boundary, FaultBoundary::HarnessModel),
        }
        assert!(!case.boundary_note.is_empty());
        assert!(!case.checks.is_empty());
    }
    let truncation = report
        .cases
        .iter()
        .find(|case| case.id == "provider-output-truncation")
        .unwrap();
    assert_eq!(truncation.final_state, "failed");
    assert!(!truncation.merged);
    let retry = report
        .cases
        .iter()
        .find(|case| case.id == "provider-stream-failure-before-output")
        .unwrap();
    assert_eq!(retry.final_state, "idle");
    assert_eq!(retry.attempts, 2);
    let cancellation = report
        .cases
        .iter()
        .find(|case| case.id == "abort-boundary-cancellation")
        .unwrap();
    assert_eq!(cancellation.final_state, "cancelled");
    assert!(!cancellation.merged);
    assert!(
        report
            .caveats
            .iter()
            .any(|caveat| caveat.contains("harness models"))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_scale_and_fault_print_pass_fail_and_json() {
    let scale = execute(BenchCommand::Scale(ScaleArgs { workers: 10 }), true)
        .await
        .unwrap();
    assert_eq!(scale.exit_code, 0);
    let scale_json = scale.json.expect("scale JSON");
    assert_eq!(scale_json["exit_code"], 0);
    assert_eq!(scale_json["report"]["schema"], SCALE_FAULT_SCHEMA);
    assert_eq!(scale_json["report"]["task_count"], SCALE_TASK_COUNT);
    assert_eq!(scale_json["report"]["completed_tasks"], SCALE_TASK_COUNT);
    assert_eq!(scale_json["report"]["passed"], true);
    assert_eq!(scale_json["report"]["checks_failed"], 0);

    let fault = execute(BenchCommand::Fault, true).await.unwrap();
    assert_eq!(fault.exit_code, 0);
    let fault_json = fault.json.expect("fault JSON");
    assert_eq!(fault_json["exit_code"], 0);
    assert_eq!(fault_json["report"]["schema"], SCALE_FAULT_SCHEMA);
    assert_eq!(fault_json["report"]["case_count"], 5);
    assert_eq!(fault_json["report"]["passed"], true);
    assert_eq!(fault_json["report"]["checks_failed"], 0);

    let human = execute(BenchCommand::Fault, false).await.unwrap();
    assert_eq!(human.exit_code, 0);
    assert!(human.human.contains("vp bench fault: PASS"));
    assert!(human.human.contains("real-runtime-fake-provider"));
    assert!(human.human.contains("harness-model-not-live-runtime"));

    let invalid = run_scale_report(0).await.unwrap_err();
    assert!(invalid.to_string().contains("workers must be 1-10"));
}

//! Deterministic process-pressure scoring.
//!
//! This module is pure: samples in, a bounded verdict out. It never signals a
//! process, reads prompts, or calls a model. The first slice uses it in
//! observe-only mode; later action classes will consume the same verdict.

use serde::{Deserialize, Serialize};

use crate::capacity::{EffectiveCaps, MachineCapacity, expected_effective_secs, learning_mode};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProcessSample {
    pub at_ms: u64,
    pub output_bytes: u64,
    pub cpu_time_ms: u64,
    pub instructions: Option<u64>,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub child_exit_count: u64,
    pub heartbeat_seq: u64,
    pub rss_bytes: u64,
    pub children: u32,
    pub open_files: u32,
    pub scratch_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessWindow {
    pub first: ProcessSample,
    pub last: ProcessSample,
    /// Seconds since any progress component last changed.
    pub last_progress_age_secs: u64,
    /// Seconds since the owning run/task/lease heartbeat changed.
    pub heartbeat_age_secs: u64,
    /// Seconds since TERM was sent without an exit; zero when TERM was not sent.
    pub term_ignored_secs: u64,
    /// Agent-declared timeout, advisory only.
    pub declared_expected_secs: Option<u64>,
    /// Observed p95 duration for this workload fingerprint, when available.
    pub calibrated_p95_secs: Option<u64>,
    /// Number of completed samples for this workload fingerprint.
    pub workload_samples: u32,
}

impl ProcessWindow {
    pub fn elapsed_secs(&self) -> u64 {
        self.last.at_ms.saturating_sub(self.first.at_ms) / 1_000
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SignalFamily {
    Resource,
    Progress,
    Control,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Normal,
    Watch,
    Pressure,
    SuspectedStall,
    Stalled,
    Runaway,
    HardCap,
    Exited,
}

impl ProcessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Watch => "watch",
            Self::Pressure => "pressure",
            Self::SuspectedStall => "suspected_stall",
            Self::Stalled => "stalled",
            Self::Runaway => "runaway",
            Self::HardCap => "hard_cap",
            Self::Exited => "exited",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    None,
    Log,
    Notice,
    FinalNotice,
    Stop,
}

impl RecommendedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Log => "log",
            Self::Notice => "notice",
            Self::FinalNotice => "final_notice",
            Self::Stop => "stop",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessVerdict {
    pub state: ProcessState,
    pub score: u8,
    pub action: RecommendedAction,
    pub families: Vec<SignalFamily>,
    pub reason: String,
    pub elapsed_secs: u64,
    pub expected_secs: u64,
    pub expected_ratio: f64,
    pub cpu_share: f64,
    pub rss_ratio: f64,
    pub no_progress_secs: u64,
    pub learning_mode: bool,
}

impl ProcessVerdict {
    pub fn is_stop(&self) -> bool {
        self.action == RecommendedAction::Stop
    }
}

pub fn evaluate(window: &ProcessWindow, capacity: &MachineCapacity) -> ProcessVerdict {
    let caps = EffectiveCaps::from_capacity(capacity);
    let elapsed_secs = window.elapsed_secs().max(1);
    let elapsed_ms = window.last.at_ms.saturating_sub(window.first.at_ms).max(1);
    let cpu_delta_ms = window
        .last
        .cpu_time_ms
        .saturating_sub(window.first.cpu_time_ms);
    let cpu_share =
        (cpu_delta_ms as f64 / elapsed_ms as f64 / capacity.logical_cores as f64).clamp(0.0, 8.0);
    let rss_ratio = (window.last.rss_bytes as f64 / capacity.physical_memory_bytes.max(1) as f64)
        .clamp(0.0, 8.0);
    let rss_growth_per_min =
        growth_per_min(window.first.rss_bytes, window.last.rss_bytes, elapsed_secs);
    let scratch_growth = window
        .last
        .scratch_bytes
        .saturating_sub(window.first.scratch_bytes);
    let expected_secs = expected_effective_secs(
        window.declared_expected_secs,
        window.calibrated_p95_secs,
        capacity,
    );
    let expected_ratio = elapsed_secs as f64 / expected_secs.max(1) as f64;
    let learning = learning_mode(window.workload_samples);

    let mut score = 0u16;
    let mut families = Vec::new();

    let cpu_points = if cpu_share >= 0.80 {
        25
    } else if cpu_share >= 0.50 {
        20
    } else {
        0
    };
    if cpu_points > 0 {
        score += cpu_points;
        push_family(&mut families, SignalFamily::Resource);
    }

    let rss_points = if rss_ratio >= 0.50 {
        35
    } else if rss_ratio >= 0.40 {
        25
    } else if rss_ratio >= 0.25 {
        15
    } else {
        0
    };
    if rss_points > 0 {
        score += rss_points;
        push_family(&mut families, SignalFamily::Resource);
    }

    if rss_growth_per_min >= (capacity.physical_memory_bytes as f64 * 0.05).max(1.0) {
        score += 30;
        push_family(&mut families, SignalFamily::Resource);
    }
    if scratch_growth >= caps.scratch_hard_effective_bytes / 2 {
        score += 20;
        push_family(&mut families, SignalFamily::Resource);
    }
    if window.last.children >= caps.children_hard_effective {
        score += 40;
        push_family(&mut families, SignalFamily::Resource);
    } else if window.last.children >= caps.children_hard_effective.saturating_mul(3) / 4 {
        score += 20;
        push_family(&mut families, SignalFamily::Resource);
    }
    if window.last.open_files >= caps.fds_hard_effective.saturating_mul(3) / 4 {
        score += 15;
        push_family(&mut families, SignalFamily::Resource);
    }

    let no_progress = window.last_progress_age_secs;
    let progress_points = if no_progress >= 900 {
        50
    } else if no_progress >= 600 {
        35
    } else if no_progress >= 300 {
        20
    } else {
        0
    };
    if progress_points > 0 {
        score += progress_points;
        push_family(&mut families, SignalFamily::Progress);
    }

    if window.heartbeat_age_secs >= 30 {
        score += 40;
        push_family(&mut families, SignalFamily::Control);
    }
    if no_progress >= 180 {
        let expected_points = if expected_ratio >= 4.0 {
            20
        } else if expected_ratio >= 2.0 {
            15
        } else if expected_ratio >= 1.5 {
            10
        } else {
            0
        };
        if expected_points > 0 {
            score += expected_points;
            push_family(&mut families, SignalFamily::Control);
        }
    }
    if window.term_ignored_secs >= 30 {
        score += 40;
        push_family(&mut families, SignalFamily::Control);
    }
    if capacity.memory_pressure == crate::capacity::MemoryPressure::Critical
        && rss_growth_per_min > 0.0
    {
        score += 20;
        push_family(&mut families, SignalFamily::Resource);
    }
    if matches!(
        capacity.thermal,
        crate::capacity::ThermalState::Serious | crate::capacity::ThermalState::Critical
    ) && cpu_share >= 0.50
    {
        score += 10;
        push_family(&mut families, SignalFamily::Resource);
    }

    let score = score.min(100) as u8;
    let hard_cap = window.last.rss_bytes >= caps.rss_hard_effective_bytes
        || window.last.children >= caps.children_hard_effective
        || window.last.scratch_bytes >= caps.scratch_hard_effective_bytes
        || window.last.open_files >= caps.fds_hard_effective;
    let growth = rss_growth_per_min >= (capacity.physical_memory_bytes as f64 * 0.05).max(1.0)
        || scratch_growth >= caps.scratch_hard_effective_bytes / 2
        || window.last.children >= caps.children_hard_effective.saturating_mul(3) / 4
        || window.last.open_files >= caps.fds_hard_effective.saturating_mul(3) / 4;

    let (mut state, mut action, mut reason) = if hard_cap {
        (
            ProcessState::HardCap,
            RecommendedAction::Stop,
            "effective hard cap exceeded".to_string(),
        )
    } else if score >= 90 && no_progress >= 600 && families.len() >= 2 && growth {
        (
            ProcessState::Runaway,
            RecommendedAction::Stop,
            "sustained resource growth with no progress".to_string(),
        )
    } else if score >= 90 && no_progress >= 600 && families.len() >= 2 {
        (
            ProcessState::Stalled,
            RecommendedAction::Stop,
            "no progress with multiple signal families".to_string(),
        )
    } else if score >= 80 && no_progress >= 300 {
        (
            ProcessState::SuspectedStall,
            RecommendedAction::FinalNotice,
            "suspected stall; final notice".to_string(),
        )
    } else if score >= 60 {
        (
            ProcessState::Pressure,
            RecommendedAction::Notice,
            "resource or progress pressure".to_string(),
        )
    } else if score >= 40 {
        (
            ProcessState::Watch,
            RecommendedAction::Log,
            "watch threshold".to_string(),
        )
    } else {
        (
            ProcessState::Normal,
            RecommendedAction::None,
            "below watch threshold".to_string(),
        )
    };

    // Recent progress is a hard guard against killing a busy build.
    if action == RecommendedAction::Stop && window.last_progress_age_secs < 60 && !hard_cap {
        state = ProcessState::SuspectedStall;
        action = RecommendedAction::FinalNotice;
        reason = "recent progress blocks stop".to_string();
    }

    // The first runs of a workload are learning mode: notices, not kills,
    // unless the process is over a hard cap or has ignored TERM after a long
    // no-progress window.
    if learning
        && action == RecommendedAction::Stop
        && !hard_cap
        && !(no_progress >= 900 && window.term_ignored_secs >= 30)
    {
        state = ProcessState::SuspectedStall;
        action = RecommendedAction::FinalNotice;
        reason = "learning mode: sample floor not reached".to_string();
    }

    ProcessVerdict {
        state,
        score,
        action,
        families,
        reason,
        elapsed_secs,
        expected_secs,
        expected_ratio,
        cpu_share,
        rss_ratio,
        no_progress_secs: no_progress,
        learning_mode: learning,
    }
}

fn push_family(families: &mut Vec<SignalFamily>, family: SignalFamily) {
    if !families.contains(&family) {
        families.push(family);
    }
}

fn growth_per_min(first: u64, last: u64, elapsed_secs: u64) -> f64 {
    if elapsed_secs == 0 {
        return 0.0;
    }
    last.saturating_sub(first) as f64 * 60.0 / elapsed_secs as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capacity::{MachineCapacity, MemoryPressure, ThermalState};

    const GIB: u64 = 1024 * 1024 * 1024;

    fn machine() -> MachineCapacity {
        MachineCapacity {
            physical_memory_bytes: 16 * GIB,
            logical_cores: 10,
            disk_free_bytes: 100 * GIB,
            disk_total_bytes: 200 * GIB,
            memory_pressure: MemoryPressure::Normal,
            thermal: ThermalState::Nominal,
            low_power: false,
        }
    }

    fn sample(at_ms: u64, cpu_time_ms: u64, output_bytes: u64, rss_bytes: u64) -> ProcessSample {
        ProcessSample {
            at_ms,
            output_bytes,
            cpu_time_ms,
            instructions: Some(cpu_time_ms * 1_000),
            disk_read_bytes: 0,
            disk_write_bytes: 0,
            child_exit_count: 0,
            heartbeat_seq: 1,
            rss_bytes,
            children: 1,
            open_files: 32,
            scratch_bytes: 0,
        }
    }

    fn window(first: ProcessSample, last: ProcessSample) -> ProcessWindow {
        ProcessWindow {
            first,
            last,
            last_progress_age_secs: 10,
            heartbeat_age_secs: 1,
            term_ignored_secs: 0,
            declared_expected_secs: Some(600),
            calibrated_p95_secs: None,
            workload_samples: 10,
        }
    }

    #[test]
    fn a_busy_build_with_progress_is_not_stopped() {
        let first = sample(0, 0, 0, GIB);
        let mut last = sample(10_000, 50_000, 0, GIB);
        last.instructions = Some(50_000_000);
        let mut window = window(first, last);
        window.last_progress_age_secs = 10;
        let verdict = evaluate(&window, &machine());
        assert!(!verdict.is_stop());
        assert!(verdict.score < 90);
    }

    #[test]
    fn a_long_flat_process_with_stale_heartbeat_is_stalled() {
        let first = sample(0, 0, 0, GIB);
        let last = sample(20 * 60_000, 0, 0, GIB);
        let mut window = window(first, last);
        window.last_progress_age_secs = 900;
        window.heartbeat_age_secs = 40;
        let verdict = evaluate(&window, &machine());
        assert_eq!(verdict.state, ProcessState::Stalled);
        assert_eq!(verdict.action, RecommendedAction::Stop);
        assert!(verdict.families.contains(&SignalFamily::Progress));
        assert!(verdict.families.contains(&SignalFamily::Control));
    }

    #[test]
    fn recent_progress_with_resource_pressure_is_only_a_notice() {
        let first = sample(0, 0, 0, GIB);
        let last = sample(20 * 60_000, 50_000, 100, 7 * GIB);
        let mut window = window(first, last);
        window.last_progress_age_secs = 30;
        window.heartbeat_age_secs = 40;
        let verdict = evaluate(&window, &machine());
        assert!(!verdict.is_stop());
        assert_eq!(verdict.action, RecommendedAction::Notice);
    }

    #[test]
    fn learning_mode_avoids_a_new_workload_kill() {
        let first = sample(0, 0, 0, GIB);
        let last = sample(20 * 60_000, 0, 0, GIB);
        let mut window = window(first, last);
        window.last_progress_age_secs = 900;
        window.heartbeat_age_secs = 40;
        window.workload_samples = 2;
        let verdict = evaluate(&window, &machine());
        assert!(!verdict.is_stop());
        assert!(verdict.learning_mode);
    }

    #[test]
    fn ignored_term_in_learning_mode_can_still_stop() {
        let first = sample(0, 0, 0, GIB);
        let last = sample(20 * 60_000, 0, 0, GIB);
        let mut window = window(first, last);
        window.last_progress_age_secs = 900;
        window.heartbeat_age_secs = 40;
        window.term_ignored_secs = 40;
        window.workload_samples = 2;
        let verdict = evaluate(&window, &machine());
        assert!(verdict.is_stop());
    }

    #[test]
    fn effective_hard_cap_stops_even_with_progress() {
        let first = sample(0, 0, 0, 9 * GIB);
        let last = sample(10_000, 10_000, 100, 9 * GIB);
        let mut window = window(first, last);
        window.last_progress_age_secs = 0;
        let verdict = evaluate(&window, &machine());
        assert_eq!(verdict.state, ProcessState::HardCap);
        assert_eq!(verdict.action, RecommendedAction::Stop);
    }
}

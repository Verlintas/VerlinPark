//! Deterministic machine-capacity normalization.
//!
//! The first slice reads physical memory, logical cores, and disk space. The
//! formulas are intentionally explicit; a later slice can add live memory
//! pressure, thermal state, and low-power detection without changing the shape
//! of the policy.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThermalState {
    Nominal,
    Fair,
    Serious,
    Critical,
}

impl ThermalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Nominal => "nominal",
            Self::Fair => "fair",
            Self::Serious => "serious",
            Self::Critical => "critical",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MachineCapacity {
    pub physical_memory_bytes: u64,
    pub logical_cores: u32,
    pub disk_free_bytes: u64,
    pub disk_total_bytes: u64,
    pub memory_pressure: MemoryPressure,
    pub thermal: ThermalState,
    pub low_power: bool,
}

impl MachineCapacity {
    pub fn detect(data_dir: &Path) -> Self {
        let physical_memory_bytes = sysctl_u64("hw.memsize").unwrap_or(8 * GIB);
        let logical_cores = std::thread::available_parallelism()
            .map(|value| value.get() as u32)
            .unwrap_or(4)
            .clamp(1, 256);
        let disk_free_bytes = fs2::available_space(data_dir).unwrap_or(0);
        let disk_total_bytes = fs2::total_space(data_dir).unwrap_or(0);
        let memory_pressure = sysctl_u64("kern.memorystatus_vm_pressure_level")
            .map(|value| match value {
                4.. => MemoryPressure::Critical,
                2..=3 => MemoryPressure::Warning,
                _ => MemoryPressure::Normal,
            })
            .unwrap_or(MemoryPressure::Normal);
        Self {
            physical_memory_bytes,
            logical_cores,
            disk_free_bytes,
            disk_total_bytes,
            memory_pressure,
            thermal: ThermalState::Nominal,
            low_power: false,
        }
    }

    pub fn memory_gib(&self) -> f64 {
        self.physical_memory_bytes as f64 / GIB as f64
    }

    pub fn disk_free_gib(&self) -> f64 {
        self.disk_free_bytes as f64 / GIB as f64
    }

    pub fn disk_used_ratio(&self) -> Option<f64> {
        if self.disk_total_bytes == 0 {
            return None;
        }
        Some(1.0 - self.disk_free_bytes as f64 / self.disk_total_bytes as f64)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EffectiveCaps {
    pub rss_hard_bytes: u64,
    pub children_hard: u32,
    pub scratch_hard_bytes: u64,
    pub fds_hard: u32,
    pub capacity_multiplier: f64,
    pub rss_hard_effective_bytes: u64,
    pub children_hard_effective: u32,
    pub scratch_hard_effective_bytes: u64,
    pub fds_hard_effective: u32,
}

impl EffectiveCaps {
    pub fn from_capacity(capacity: &MachineCapacity) -> Self {
        let mem_gib = capacity.memory_gib().max(0.0);
        let disk_free_gib = capacity.disk_free_gib().max(0.0);
        let rss_hard_bytes = gib(clamp_f64(0.50 * mem_gib, 3.0, 24.0));
        let children_hard = clamp_u32(16 * capacity.logical_cores, 64, 256);
        let scratch_hard_bytes = gib(clamp_f64(0.05 * disk_free_gib, 2.0, 24.0));
        let fds_hard = clamp_u32(1024 * capacity.logical_cores, 4096, 32768);

        let mut capacity_multiplier: f64 = 1.0;
        if capacity.low_power {
            capacity_multiplier += 0.25;
        }
        capacity_multiplier += match capacity.thermal {
            ThermalState::Nominal => 0.0,
            ThermalState::Fair => 0.0,
            ThermalState::Serious => 0.25,
            ThermalState::Critical => 0.50,
        };
        let capacity_multiplier = capacity_multiplier.clamp(1.0, 2.0);

        let rss_hard_effective_bytes = match capacity.memory_pressure {
            MemoryPressure::Critical => rss_hard_bytes.min(gib(0.40 * mem_gib)),
            _ => rss_hard_bytes,
        };
        let children_hard_effective = match capacity.memory_pressure {
            MemoryPressure::Critical => children_hard.saturating_mul(3) / 4,
            _ => children_hard,
        };
        let fds_hard_effective = match capacity.memory_pressure {
            MemoryPressure::Critical => fds_hard.saturating_mul(3) / 4,
            _ => fds_hard,
        };
        let scratch_hard_effective_bytes =
            scratch_hard_bytes.min((capacity.disk_free_bytes / 10).max(GIB));

        Self {
            rss_hard_bytes,
            children_hard,
            scratch_hard_bytes,
            fds_hard,
            capacity_multiplier,
            rss_hard_effective_bytes,
            children_hard_effective,
            scratch_hard_effective_bytes,
            fds_hard_effective,
        }
    }
}

pub fn expected_effective_secs(
    declared_secs: Option<u64>,
    calibrated_p95_secs: Option<u64>,
    capacity: &MachineCapacity,
) -> u64 {
    let base = calibrated_p95_secs
        .map(|p95| p95.saturating_mul(2))
        .or(declared_secs)
        .unwrap_or(600);
    let scaled = (base as f64 * capacity_multiplier(capacity)).round() as u64;
    scaled.clamp(30, 8 * 60 * 60)
}

pub fn capacity_multiplier(capacity: &MachineCapacity) -> f64 {
    let mut multiplier: f64 = 1.0;
    if capacity.low_power {
        multiplier += 0.25;
    }
    multiplier += match capacity.thermal {
        ThermalState::Nominal | ThermalState::Fair => 0.0,
        ThermalState::Serious => 0.25,
        ThermalState::Critical => 0.50,
    };
    multiplier.clamp(1.0, 2.0)
}

pub fn learning_mode(samples: u32) -> bool {
    samples < 5
}

fn sysctl_u64(name: &str) -> Option<u64> {
    let candidates = ["/usr/sbin/sysctl", "sysctl"];
    for candidate in candidates {
        let Ok(output) = Command::new(candidate).arg("-n").arg(name).output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        if let Ok(value) = text.trim().parse::<u64>() {
            return Some(value);
        }
    }
    None
}

fn gib(value: f64) -> u64 {
    (value.max(0.0) * GIB as f64).round() as u64
}

fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn clamp_u32(value: u32, min: u32, max: u32) -> u32 {
    value.max(min).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capacity(mem_gib: u64, cores: u32, disk_gib: u64) -> MachineCapacity {
        MachineCapacity {
            physical_memory_bytes: mem_gib * GIB,
            logical_cores: cores,
            disk_free_bytes: disk_gib * GIB,
            disk_total_bytes: disk_gib * 2 * GIB,
            memory_pressure: MemoryPressure::Normal,
            thermal: ThermalState::Nominal,
            low_power: false,
        }
    }

    #[test]
    fn caps_scale_with_machine() {
        let small = EffectiveCaps::from_capacity(&capacity(8, 8, 100));
        assert_eq!(small.rss_hard_bytes, 4 * GIB);
        assert_eq!(small.children_hard, 128);
        assert_eq!(small.fds_hard, 8192);
        assert_eq!(small.scratch_hard_bytes, 5 * GIB);

        let large = EffectiveCaps::from_capacity(&capacity(64, 16, 500));
        assert_eq!(large.rss_hard_bytes, 24 * GIB);
        assert_eq!(large.children_hard, 256);
        assert_eq!(large.fds_hard, 16384);
        assert_eq!(large.scratch_hard_bytes, 24 * GIB);
    }

    #[test]
    fn critical_pressure_tightens_caps() {
        let mut machine = capacity(16, 10, 100);
        machine.memory_pressure = MemoryPressure::Critical;
        let caps = EffectiveCaps::from_capacity(&machine);
        assert!(caps.rss_hard_effective_bytes < caps.rss_hard_bytes);
        assert!(caps.children_hard_effective < caps.children_hard);
        assert!(caps.fds_hard_effective < caps.fds_hard);
    }

    #[test]
    fn expected_uses_calibration_and_capacity_multiplier() {
        let mut machine = capacity(16, 10, 100);
        assert_eq!(expected_effective_secs(Some(900), Some(120), &machine), 240);
        machine.thermal = ThermalState::Serious;
        assert_eq!(expected_effective_secs(Some(900), Some(120), &machine), 300);
        assert_eq!(expected_effective_secs(Some(900), None, &machine), 1125);
    }

    #[test]
    fn learning_mode_requires_five_samples() {
        assert!(learning_mode(0));
        assert!(learning_mode(4));
        assert!(!learning_mode(5));
    }
}

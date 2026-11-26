//! The fixed Custos charter surface.
//!
//! This is deliberately not runtime policy. The signed release will replace
//! the draft canonical text and hash; no GUI, RPC, project row, environment
//! variable, or agent can edit it.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::protocol::CUSTOS_PROTOCOL_VERSION;

pub const CHARTER_VERSION: &str = "0-draft";
pub const CHARTER_STATUS: &str = "draft";

/// A compact canonical statement of the charter. The full prose lives in
/// `docs/CUSTOS_CHARTER_V0.md`; this is the part the runtime pins and exposes.
const CHARTER_CANONICAL: &str = "\
CUSTOS_CHARTER/0-draft\n\
Custos is a deterministic integrity guardian. It does not call a language model.\n\
Authority: L0 observe; L1 notify; L2 safe repair; L3 containment; L4 recovery; L5 prohibited.\n\
V1 is observe-only. It may inspect known daemon-owned state and processes.\n\
It never approves approvals, merges, verifies work, changes budgets/capacity/routing,\n\
edits identities, reads secrets, edits VerlinPark source/policy, or kills foreign processes.\n\
The user has awareness, not jurisdiction. The charter changes only through a signed release.\n";

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionClass {
    Observe,
    Notify,
    SafeRepair,
    Containment,
    Recovery,
    Prohibited,
}

impl ActionClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Notify => "notify",
            Self::SafeRepair => "safe_repair",
            Self::Containment => "containment",
            Self::Recovery => "recovery",
            Self::Prohibited => "prohibited",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ActionSpec {
    pub id: &'static str,
    pub class: ActionClass,
    pub summary: &'static str,
    pub mutates: bool,
}

/// Closed v0 action catalog. `observe_only` permits only the OBS rows.
pub const ACTIONS: &[ActionSpec] = &[
    ActionSpec {
        id: "OBS-01",
        class: ActionClass::Observe,
        summary: "probe daemon discovery and database",
        mutates: false,
    },
    ActionSpec {
        id: "OBS-02",
        class: ActionClass::Observe,
        summary: "probe sessions, runs, leases, queues, effects",
        mutates: false,
    },
    ActionSpec {
        id: "OBS-03",
        class: ActionClass::Observe,
        summary: "probe known processes and heartbeat",
        mutates: false,
    },
    ActionSpec {
        id: "NTC-01",
        class: ActionClass::Notify,
        summary: "send a sealed one-way custos.notice",
        mutates: false,
    },
    ActionSpec {
        id: "RPR-01",
        class: ActionClass::SafeRepair,
        summary: "reclaim an expired runtime lease",
        mutates: true,
    },
    ActionSpec {
        id: "RPR-02",
        class: ActionClass::SafeRepair,
        summary: "release a stale queue claim",
        mutates: true,
    },
    ActionSpec {
        id: "RPR-03",
        class: ActionClass::SafeRepair,
        summary: "reattach an interrupted run",
        mutates: true,
    },
    ActionSpec {
        id: "CTN-01",
        class: ActionClass::Containment,
        summary: "pause or cancel a stuck session",
        mutates: true,
    },
    ActionSpec {
        id: "CTN-02",
        class: ActionClass::Containment,
        summary: "stop a known process group",
        mutates: true,
    },
    ActionSpec {
        id: "REC-01",
        class: ActionClass::Recovery,
        summary: "restart the daemon after a verified hang",
        mutates: true,
    },
    ActionSpec {
        id: "REC-02",
        class: ActionClass::Recovery,
        summary: "restore a verified backup",
        mutates: true,
    },
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionView {
    pub id: String,
    pub class: String,
    pub summary: String,
    pub mutates: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CharterView {
    pub schema: String,
    pub version: String,
    pub status: String,
    pub hash: String,
    pub protocol: u32,
    pub observe_only: bool,
    pub actions: Vec<ActionView>,
}

pub fn hash() -> String {
    let digest = Sha256::digest(CHARTER_CANONICAL.as_bytes());
    format!("sha256:{}", hex(&digest))
}

pub fn view(observe_only: bool) -> CharterView {
    CharterView {
        schema: "verlinpark.custos_charter/v1".to_string(),
        version: CHARTER_VERSION.to_string(),
        status: CHARTER_STATUS.to_string(),
        hash: hash(),
        protocol: CUSTOS_PROTOCOL_VERSION,
        observe_only,
        actions: ACTIONS
            .iter()
            .map(|action| ActionView {
                id: action.id.to_string(),
                class: action.class.as_str().to_string(),
                summary: action.summary.to_string(),
                mutates: action.mutates,
                enabled: enabled(action, observe_only),
            })
            .collect(),
    }
}

pub fn allows(action_id: &str, observe_only: bool) -> bool {
    ACTIONS
        .iter()
        .find(|action| action.id == action_id)
        .is_some_and(|action| enabled(action, observe_only))
}

fn enabled(action: &ActionSpec, observe_only: bool) -> bool {
    if observe_only {
        action.class == ActionClass::Observe
    } else {
        action.class != ActionClass::Prohibited
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charter_hash_is_stable() {
        assert_eq!(hash(), hash());
        assert!(hash().starts_with("sha256:"));
        assert_eq!(hash().len(), 71);
    }

    #[test]
    fn observe_only_allows_observe_and_denies_mutation() {
        assert!(allows("OBS-01", true));
        assert!(!allows("NTC-01", true));
        assert!(!allows("RPR-01", true));
        assert!(!allows("CTN-02", true));
        assert!(!allows("REC-02", true));
        assert!(!allows("NOPE", true));
    }

    #[test]
    fn non_observe_mode_still_has_no_prohibited_action() {
        assert!(allows("RPR-01", false));
        assert!(allows("CTN-02", false));
        assert_eq!(
            ACTIONS
                .iter()
                .filter(|action| action.class == ActionClass::Prohibited)
                .count(),
            0
        );
    }

    #[test]
    fn view_marks_only_observe_enabled() {
        let view = view(true);
        assert!(view.observe_only);
        assert_eq!(
            view.actions.iter().filter(|action| action.enabled).count(),
            3
        );
        assert_eq!(view.hash, hash());
    }
}

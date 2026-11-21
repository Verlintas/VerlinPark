//! Database opening, schema creation, and additive migrations.
//!
//! `open` is the only place that creates tables; order is preserved from the
//! original monolithic module.

use super::*;

impl Store {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref().to_path_buf();
        paths::ensure_private_dir(&data_dir)?;
        paths::ensure_private_dir(&data_dir.join("run"))?;
        paths::ensure_private_dir(&data_dir.join("sessions"))?;
        paths::ensure_private_dir(&data_dir.join("logs"))?;

        let conn = Connection::open(data_dir.join("verlinpark.db"))
            .with_context(|| format!("opening database in {}", data_dir.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS store_meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS projects (
                id            TEXT PRIMARY KEY,
                name          TEXT NOT NULL,
                root          TEXT NOT NULL UNIQUE,
                default_branch TEXT NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS model_presets (
                id                    TEXT PRIMARY KEY,
                name                  TEXT NOT NULL,
                provider              TEXT NOT NULL,
                model                 TEXT NOT NULL,
                context_window        INTEGER,
                input_usd_per_million REAL,
                output_usd_per_million REAL,
                notes                 TEXT NOT NULL DEFAULT '',
                created_at            TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_shell_policies (
                project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                network         TEXT NOT NULL,
                read_deny       TEXT NOT NULL,
                restrict_home   INTEGER NOT NULL DEFAULT 0,
                read_jail       INTEGER NOT NULL DEFAULT 0,
                updated_at      TEXT NOT NULL
            );

            -- Project-scoped task-contract policy. A missing row means the
            -- enabled/enforce default; it never means "no policy".
            CREATE TABLE IF NOT EXISTS project_contract_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Project setting for durable Leader/Manager/Worker identities.
            -- A missing row means disabled, so upgrading never enables the
            -- program implicitly.
            CREATE TABLE IF NOT EXISTS project_agent_identity_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Project-scoped effect-receipt policy. A missing row means the
            -- enabled, unknown-blocks-merge, 120s-grace default.
            CREATE TABLE IF NOT EXISTS project_effect_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Canonical task contracts. A row is a projection of the matching
            -- `contract.derived` event; replay/repair/db-rebuild recreate it.
            CREATE TABLE IF NOT EXISTS contracts (
                contract_id   TEXT PRIMARY KEY,
                project_id    TEXT NOT NULL,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                task_id       TEXT,
                plan_revision INTEGER NOT NULL,
                role          TEXT NOT NULL,
                hash          TEXT NOT NULL,
                source_hash   TEXT NOT NULL,
                contract      TEXT NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_contracts_session
                ON contracts(session_id, created_at);

            -- Immutable project policy bundles referenced by plan/run runtime
            -- snapshots. Rows are content-addressed and deduplicated per
            -- project; only canonical policy JSON is stored (no prompts,
            -- provider keys, headers, or file content).
            CREATE TABLE IF NOT EXISTS policy_snapshots (
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                hash       TEXT NOT NULL,
                json       TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (project_id, hash)
            );

            CREATE INDEX IF NOT EXISTS idx_policy_snapshots_project
                ON policy_snapshots(project_id, created_at);

            -- Per-contract mutation usage derived from `intent.mutation`
            -- events. It is a projection, not an independent authority.
            CREATE TABLE IF NOT EXISTS contract_usage (
                contract_id TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                task_id     TEXT,
                writes      INTEGER NOT NULL DEFAULT 0,
                bytes       INTEGER NOT NULL DEFAULT 0,
                commits     INTEGER NOT NULL DEFAULT 0,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_contract_usage_session
                ON contract_usage(session_id);

            -- Local effect receipts. A row is a projection of the matching
            -- `effect.*` event; replay/repair/db-rebuild recreate it.
            -- `done` is terminal; `unknown` requires explicit reconcile;
            -- `failed` may retry as a new attempt row. The partial unique
            -- index makes one logical effect idempotent across retries while
            -- still allowing a failed attempt to be recorded.
            CREATE TABLE IF NOT EXISTS effect_receipts (
                id            TEXT PRIMARY KEY,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                task_id       TEXT,
                attempt       INTEGER NOT NULL,
                tool          TEXT NOT NULL,
                target        TEXT NOT NULL,
                request_hash  TEXT NOT NULL,
                state         TEXT NOT NULL,
                provider_key  TEXT,
                external_id   TEXT,
                response_hash TEXT,
                evidence_json TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                resolved_at   TEXT,
                settled_by_sweep INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_effect_receipts_session
                ON effect_receipts(session_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_effect_receipts_task
                ON effect_receipts(session_id, task_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_effect_receipts_lookup
                ON effect_receipts(session_id, tool, target, request_hash, attempt);
            CREATE INDEX IF NOT EXISTS idx_effect_receipts_state_created
                ON effect_receipts(state, created_at);

            CREATE UNIQUE INDEX IF NOT EXISTS idx_effect_receipts_idempotent
                ON effect_receipts(
                    session_id,
                    COALESCE(task_id, ''),
                    tool,
                    target,
                    request_hash
                )
                WHERE state IN ('intent', 'done', 'unknown');

            -- Project-scoped external-gate policy. A missing row means
            -- local-only resolution and the loopback endpoint disabled.
            CREATE TABLE IF NOT EXISTS project_gate_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Project-scoped OTel GenAI export policy. A missing row means
            -- export is disabled and the endpoint stays loopback-only.
            CREATE TABLE IF NOT EXISTS project_otel_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- External gates. A row is a projection of the matching `gate.*`
            -- event. Only the SHA-256 verifier of the one-time token is stored;
            -- the raw token is returned once by gate.open and never persisted.
            CREATE TABLE IF NOT EXISTS external_gates (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                task_id         TEXT,
                kind            TEXT NOT NULL,
                label           TEXT NOT NULL,
                token_hash      TEXT NOT NULL,
                state           TEXT NOT NULL,
                outcome         TEXT,
                payload_json    TEXT NOT NULL DEFAULT '{}',
                resolution_note TEXT,
                created_at      TEXT NOT NULL,
                expires_at      TEXT,
                resolved_at     TEXT,
                resolved_by     TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_external_gates_session
                ON external_gates(session_id, state, created_at);

            CREATE INDEX IF NOT EXISTS idx_external_gates_expiry
                ON external_gates(state, expires_at);

            -- Project-scoped MCP tool schema pins. Approved hashes are never
            -- overwritten by discovery; observed hashes record drift.
            CREATE TABLE IF NOT EXISTS mcp_tool_pins (
                project_id                 TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                server_id                  TEXT NOT NULL,
                server_name                TEXT NOT NULL,
                tool_name                  TEXT NOT NULL,
                server_fingerprint         TEXT NOT NULL,
                schema_hash                TEXT NOT NULL,
                description_hash           TEXT NOT NULL,
                observed_schema_hash       TEXT NOT NULL,
                observed_description_hash  TEXT NOT NULL,
                observed_server_fingerprint TEXT NOT NULL,
                status                     TEXT NOT NULL,
                approved_at                TEXT,
                approved_by                TEXT,
                last_seen_at               TEXT NOT NULL,
                description_preview        TEXT NOT NULL,
                PRIMARY KEY (project_id, server_id, tool_name)
            );

            CREATE INDEX IF NOT EXISTS idx_mcp_tool_pins_server
                ON mcp_tool_pins(project_id, server_id);

            -- Durable MCP asynchronous task handles. This is operational state
            -- backed by session events; the remote task itself remains owned by
            -- the MCP server.
            CREATE TABLE IF NOT EXISTS mcp_tasks (
                id                TEXT PRIMARY KEY,
                project_id        TEXT NOT NULL DEFAULT '',
                session_id        TEXT NOT NULL DEFAULT '',
                agent_id          TEXT NOT NULL DEFAULT '',
                agent_name        TEXT NOT NULL DEFAULT '',
                task_id           TEXT,
                server_id         TEXT NOT NULL,
                server_name       TEXT NOT NULL,
                tool_name         TEXT NOT NULL,
                remote_task_id    TEXT NOT NULL,
                status            TEXT NOT NULL,
                ttl_secs          INTEGER NOT NULL DEFAULT 600,
                poll_interval_ms  INTEGER NOT NULL DEFAULT 1000,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                expires_at        TEXT,
                result_json       TEXT,
                result_hash       TEXT,
                result_bytes      INTEGER NOT NULL DEFAULT 0,
                effect_receipt_id TEXT,
                last_error        TEXT,
                UNIQUE (session_id, server_id, remote_task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_mcp_tasks_session
                ON mcp_tasks(session_id, status, updated_at);
            CREATE INDEX IF NOT EXISTS idx_mcp_tasks_project
                ON mcp_tasks(project_id, updated_at);


            CREATE TABLE IF NOT EXISTS project_process_policies (
                project_id          TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                idle_sweep_enabled  INTEGER NOT NULL DEFAULT 1,
                max_processes       INTEGER NOT NULL DEFAULT 8,
                idle_ttl_secs       INTEGER NOT NULL DEFAULT 3600,
                max_output_bytes    INTEGER NOT NULL DEFAULT 1048576,
                max_output_lines    INTEGER NOT NULL DEFAULT 20000,
                restart_on_boot     INTEGER NOT NULL DEFAULT 0,
                restart_max_attempts INTEGER NOT NULL DEFAULT 3,
                restart_backoff_secs INTEGER NOT NULL DEFAULT 5,
                updated_at          TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_preemption_policies (
                project_id              TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled                 INTEGER NOT NULL DEFAULT 1,
                provider_grace_ms       INTEGER NOT NULL DEFAULT 2000,
                tool_grace_ms           INTEGER NOT NULL DEFAULT 5000,
                kill_escalation_ms      INTEGER NOT NULL DEFAULT 2000,
                tool_deadline_secs      INTEGER,
                checkpoint_on_preempt   INTEGER NOT NULL DEFAULT 1,
                updated_at              TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_cache_policies (
                project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy          TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            -- Content-addressed instruction bodies. Identical section text is
            -- stored once and shared across sessions/agents. The text is
            -- operational prompt material, never cache telemetry.
            CREATE TABLE IF NOT EXISTS instruction_blobs (
                hash       TEXT PRIMARY KEY,
                text       TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            -- Ordered instruction references for one instruction scope. The
            -- scope is normally the Leader session id; task-scoped agents use
            -- `<session_id>#<agent_id>` so the deterministic key vocabulary is
            -- preserved without mixing role charters. No FK: scoped ids are
            -- intentionally not sessions and are cleaned with their session.
            CREATE TABLE IF NOT EXISTS session_instruction_state (
                session_id TEXT NOT NULL,
                key        TEXT NOT NULL,
                hash       TEXT NOT NULL,
                ordinal    INTEGER NOT NULL,
                tail       INTEGER NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, key)
            );
            CREATE INDEX IF NOT EXISTS idx_session_instruction_state_session
                ON session_instruction_state(session_id);

            -- Per-session provider observations of one stable cache epoch.
            -- Epoch identity excludes session/run/prompt/memory/tail content,
            -- so multiple sessions legitimately share a row per epoch.
            CREATE TABLE IF NOT EXISTS prompt_prefix_observations (
                project_id               TEXT NOT NULL,
                cache_epoch              TEXT NOT NULL,
                session_id               TEXT NOT NULL,
                model                    TEXT NOT NULL,
                tool_bundle              TEXT NOT NULL,
                tool_bundle_hash         TEXT NOT NULL,
                core_hash                TEXT NOT NULL,
                project_hash             TEXT NOT NULL,
                core_sections            TEXT NOT NULL,
                project_sections         TEXT NOT NULL,
                first_seen               TEXT NOT NULL,
                last_seen                TEXT NOT NULL,
                observed_input_tokens    INTEGER NOT NULL,
                observed_cache_read_tokens INTEGER NOT NULL,
                calls                    INTEGER NOT NULL,
                PRIMARY KEY (project_id, cache_epoch, session_id)
            );
            CREATE INDEX IF NOT EXISTS idx_prompt_prefix_observations_session
                ON prompt_prefix_observations(project_id, session_id);
            CREATE INDEX IF NOT EXISTS idx_prompt_prefix_observations_epoch
                ON prompt_prefix_observations(project_id, cache_epoch);

            -- Aggregate projection rebuilt from prompt_prefix_observations.
            -- It is never derived from a session's current configuration.
            CREATE TABLE IF NOT EXISTS prompt_prefixes (
                project_id               TEXT NOT NULL,
                cache_epoch              TEXT NOT NULL,
                model                    TEXT NOT NULL,
                tool_bundle              TEXT NOT NULL,
                tool_bundle_hash         TEXT NOT NULL,
                core_hash                TEXT NOT NULL,
                project_hash             TEXT NOT NULL,
                core_sections            TEXT NOT NULL,
                project_sections         TEXT NOT NULL,
                first_seen               TEXT NOT NULL,
                last_seen                TEXT NOT NULL,
                observed_input_tokens    INTEGER NOT NULL,
                observed_cache_read_tokens INTEGER NOT NULL,
                session_count            INTEGER NOT NULL,
                call_count               INTEGER NOT NULL,
                PRIMARY KEY (project_id, cache_epoch)
            );
            CREATE INDEX IF NOT EXISTS idx_prompt_prefixes_last_seen
                ON prompt_prefixes(project_id, last_seen);

            CREATE TABLE IF NOT EXISTS project_execution_hosts (
                project_id      TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                config          TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_integration_policies (
                project_id         TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                auto_resolve       INTEGER NOT NULL,
                max_auto_attempts  INTEGER NOT NULL,
                max_resolve_tokens INTEGER,
                updated_at         TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_gui_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Project-scoped default policy for deterministic auto budgets. A
            -- missing row means the disabled default; auto session creation
            -- then requires an explicit session policy with a hard cap.
            CREATE TABLE IF NOT EXISTS project_auto_budget_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_project_auto_budget_policies_updated
                ON project_auto_budget_policies(updated_at DESC);

            -- Bounded project-policy audit trail. Project configuration has no
            -- session event stream, so this is the project-scoped event record
            -- for `project.auto_budget.updated` and is carried by the manifest.
            CREATE TABLE IF NOT EXISTS project_auto_budget_events (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE(project_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_project_auto_budget_events_project
                ON project_auto_budget_events(project_id, seq DESC);

            CREATE TABLE IF NOT EXISTS sessions (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                goal       TEXT NOT NULL,
                status        TEXT NOT NULL,
                priority      INTEGER NOT NULL DEFAULT 0,
                worktree      TEXT,
                branch        TEXT,
                token_budget  INTEGER,
                cost_budget_usd REAL,
                time_budget_secs INTEGER,
                unlimited     INTEGER NOT NULL DEFAULT 0,
                cost_spent_usd REAL NOT NULL DEFAULT 0,
                base_commit   TEXT,
                team_plan     TEXT,
                plan_runtime_snapshot TEXT,
                tokens_in     INTEGER NOT NULL DEFAULT 0,
                tokens_out    INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL
            );

            -- Durable auto-budget mode/policy/operational state. Effective
            -- budgets remain on `sessions`; hard caps are copied here so a
            -- later project-policy edit cannot widen an existing session.
            CREATE TABLE IF NOT EXISTS session_auto_budgets (
                session_id  TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                mode        TEXT NOT NULL,
                policy_json TEXT NOT NULL,
                state_json  TEXT NOT NULL,
                caps_json   TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_session_auto_budgets_mode
                ON session_auto_budgets(mode);

            CREATE TABLE IF NOT EXISTS events (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                type       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (session_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_events_session_seq
                ON events(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_events_session_type_seq
                ON events(session_id, type, seq);
            CREATE INDEX IF NOT EXISTS idx_events_type_created
                ON events(type, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_sessions_project
                ON sessions(project_id);

            CREATE TABLE IF NOT EXISTS agents (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role       TEXT NOT NULL,
                name       TEXT NOT NULL DEFAULT 'Agent',
                title      TEXT,
                profile    TEXT,
                parent_id  TEXT,
                worktree   TEXT,
                branch     TEXT,
                status     TEXT NOT NULL,
                model      TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            -- Project-scoped employee records. Identity/run separation begins
            -- here: `agents` stays the run/roster projection, and a run may
            -- only point at an identity when that assignment is made by a
            -- later phase.
            CREATE TABLE IF NOT EXISTS agent_identities (
                id                TEXT PRIMARY KEY,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                role              TEXT NOT NULL,
                department        TEXT,
                component         TEXT,
                title             TEXT,
                profile           TEXT,
                model             TEXT,
                parent_identity_id TEXT,
                status            TEXT NOT NULL,
                generation        INTEGER NOT NULL DEFAULT 1,
                created_at        TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                retired_at        TEXT,
                successor_id      TEXT,
                policy_json       TEXT NOT NULL DEFAULT '{}',
                origin            TEXT NOT NULL DEFAULT 'local'
            );

            CREATE INDEX IF NOT EXISTS idx_agent_identities_project_status
                ON agent_identities(project_id, status, created_at);
            CREATE INDEX IF NOT EXISTS idx_agent_identities_project_role
                ON agent_identities(project_id, role, status);
            -- One live Leader per project. Retirement releases the slot; no
            -- automatic successor is ever created.
            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_identities_one_live_leader
                ON agent_identities(project_id)
                WHERE role = 'leader' AND status <> 'retired';

            -- Project-scoped append-only lifecycle stream. There is no session
            -- id: an identity outlives the sessions that may later use it.
            CREATE TABLE IF NOT EXISTS agent_identity_events (
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id TEXT NOT NULL,
                seq         INTEGER NOT NULL,
                kind        TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                PRIMARY KEY (identity_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_agent_identity_events_project
                ON agent_identity_events(project_id, created_at, identity_id, seq);

            -- Session→identity assignment and identity lease. A session may use
            -- several identities (Leader/Managers/Workers), but v1 permits one
            -- active session per identity. The partial unique index is the
            -- database backstop; the store also checks the same invariant in a
            -- transaction so the failure is a typed conflict, not a raw SQL
            -- constraint. `queued` is reserved and never produced by S2.
            CREATE TABLE IF NOT EXISTS session_agent_assignments (
                session_id       TEXT NOT NULL,
                identity_id      TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,
                role             TEXT NOT NULL,
                status           TEXT NOT NULL,
                lease_token      TEXT,
                lease_expires_at TEXT,
                assigned_at      TEXT NOT NULL,
                released_at      TEXT,
                heartbeat_at     TEXT NOT NULL,
                PRIMARY KEY (session_id, identity_id)
            );

            CREATE INDEX IF NOT EXISTS idx_session_agent_assignments_session
                ON session_agent_assignments(session_id, status, assigned_at);
            CREATE INDEX IF NOT EXISTS idx_session_agent_assignments_identity
                ON session_agent_assignments(identity_id, status, lease_expires_at);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_session_agent_assignments_one_active
                ON session_agent_assignments(identity_id)
                WHERE status = 'active';

            -- Project-scoped identity inbox. Labour, Verifier, and Scout do not
            -- have project identities and therefore never use this table; their
            -- task-scoped delivery is separate.
            CREATE TABLE IF NOT EXISTS agent_inbox (
                id               TEXT PRIMARY KEY,
                project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id      TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,
                session_id       TEXT,
                from_identity_id TEXT,
                kind             TEXT NOT NULL,
                payload_json     TEXT NOT NULL,
                status           TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                read_at          TEXT,
                expires_at       TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_agent_inbox_identity
                ON agent_inbox(project_id, identity_id, status, created_at);
            CREATE INDEX IF NOT EXISTS idx_agent_inbox_session
                ON agent_inbox(project_id, session_id, status, created_at);
            CREATE INDEX IF NOT EXISTS idx_agent_inbox_expiry
                ON agent_inbox(status, expires_at);

            -- Project-scoped append-only coordination stream for assignments and
            -- inbox transitions. `seq` orders replay within one project; `id`
            -- is portable across databases so archive import can be idempotent.
            CREATE TABLE IF NOT EXISTS agent_coordination_events (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE (project_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_agent_coordination_events_project
                ON agent_coordination_events(project_id, seq);

            -- Durable Worker component workspaces. The branch is the long-lived
            -- component branch; a session attaches it and gets its own session
            -- branch/worktree. One active row per (project, identity, component)
            -- is a hard invariant. One live component lease per project/component
            -- is a second partial unique index so two identities or sessions can
            -- never mutate the same component branch concurrently.
            CREATE TABLE IF NOT EXISTS agent_workspaces (
                id               TEXT PRIMARY KEY,
                project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id      TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,
                component        TEXT NOT NULL,
                branch           TEXT NOT NULL,
                base_commit      TEXT NOT NULL,
                worktree_path    TEXT NOT NULL,
                status           TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                last_session_id  TEXT,
                lease_token      TEXT,
                lease_expires_at TEXT,
                origin           TEXT NOT NULL DEFAULT 'local'
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_workspaces_active_identity_component
                ON agent_workspaces(project_id, identity_id, component)
                WHERE status = 'active';
            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_workspaces_one_component_lease
                ON agent_workspaces(project_id, component)
                WHERE lease_token IS NOT NULL;
            CREATE INDEX IF NOT EXISTS idx_agent_workspaces_project_status
                ON agent_workspaces(project_id, status, updated_at);
            CREATE INDEX IF NOT EXISTS idx_agent_workspaces_lease_expiry
                ON agent_workspaces(status, lease_expires_at);

            -- One row per (session, identity); the row is reactivated on a later
            -- attach instead of inventing duplicate binding history.
            CREATE TABLE IF NOT EXISTS session_worker_workspaces (
                id               TEXT PRIMARY KEY,
                workspace_id     TEXT NOT NULL REFERENCES agent_workspaces(id) ON DELETE CASCADE,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                identity_id      TEXT NOT NULL,
                worktree_path    TEXT NOT NULL,
                branch           TEXT NOT NULL,
                base_commit      TEXT NOT NULL,
                status           TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                lease_token      TEXT,
                lease_expires_at TEXT,
                UNIQUE (session_id, identity_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_session_worker_workspaces_one_active
                ON session_worker_workspaces(session_id, identity_id)
                WHERE status = 'active';
            CREATE INDEX IF NOT EXISTS idx_session_worker_workspaces_workspace
                ON session_worker_workspaces(workspace_id, status);

            -- Append-only project workspace stream. Portable by event id for
            -- idempotent archive import; seq is the per-project replay order.
            CREATE TABLE IF NOT EXISTS agent_workspace_events (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE (project_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_agent_workspace_events_project
                ON agent_workspace_events(project_id, seq);

            -- Project-scoped identity/department/project memory (S5). This is
            -- deliberately separate from the session-scoped `memories` table:
            -- scope, evidence, TTL, supersede edges, and provenance are durable
            -- project state. Retirement does not delete rows; it makes agent
            -- memory read-only. Imported rows are read-only evidence.
            CREATE TABLE IF NOT EXISTS agent_memories (
                id            TEXT PRIMARY KEY,
                project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id   TEXT REFERENCES agent_identities(id) ON DELETE CASCADE,
                department    TEXT,
                component     TEXT,
                scope         TEXT NOT NULL,
                content       TEXT NOT NULL,
                content_hash  TEXT NOT NULL,
                evidence_json TEXT NOT NULL DEFAULT '[]',
                source        TEXT NOT NULL,
                status        TEXT NOT NULL,
                pinned        INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                expires_at    TEXT,
                verified_at   TEXT,
                superseded_by TEXT,
                stale_reason  TEXT,
                origin        TEXT NOT NULL DEFAULT 'local'
            );

            CREATE INDEX IF NOT EXISTS idx_agent_memories_project_scope
                ON agent_memories(project_id, scope, status, updated_at);
            CREATE INDEX IF NOT EXISTS idx_agent_memories_identity
                ON agent_memories(project_id, identity_id, status, updated_at);
            CREATE INDEX IF NOT EXISTS idx_agent_memories_department
                ON agent_memories(project_id, department, status, updated_at);
            CREATE INDEX IF NOT EXISTS idx_agent_memories_expiry
                ON agent_memories(status, expires_at);
            CREATE INDEX IF NOT EXISTS idx_agent_memories_hash
                ON agent_memories(project_id, content_hash);

            -- Append-only project memory stream. Portable by event id for
            -- idempotent archive import; seq is the per-project replay order.
            CREATE TABLE IF NOT EXISTS agent_memory_events (
                id         TEXT PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                seq        INTEGER NOT NULL,
                kind       TEXT NOT NULL,
                payload    TEXT NOT NULL,
                created_at TEXT NOT NULL,
                UNIQUE (project_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_agent_memory_events_project
                ON agent_memory_events(project_id, seq);

            -- Project-scoped retirement thresholds (S6). Default is disabled;
            -- this row never fires an identity by itself.
            CREATE TABLE IF NOT EXISTS project_agent_retirement_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Explicit hire requests. A pending row reserves an identity id but
            -- creates no identity, session, assignment, workspace, or memory.
            -- Approval is the only path from pending to a created identity.
            CREATE TABLE IF NOT EXISTS agent_hire_requests (
                id                 TEXT PRIMARY KEY,
                project_id         TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id        TEXT NOT NULL,
                role               TEXT NOT NULL,
                department         TEXT,
                component          TEXT,
                title              TEXT,
                profile            TEXT,
                model              TEXT,
                parent_identity_id TEXT,
                policy_json        TEXT NOT NULL DEFAULT '{}',
                status             TEXT NOT NULL,
                requested_at       TEXT NOT NULL,
                requested_by       TEXT NOT NULL,
                approval_risk      TEXT NOT NULL,
                decided_at         TEXT,
                decided_by         TEXT,
                reason             TEXT,
                origin             TEXT NOT NULL DEFAULT 'local'
            );

            CREATE INDEX IF NOT EXISTS idx_agent_hire_requests_project
                ON agent_hire_requests(project_id, status, requested_at);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_hire_requests_one_pending_identity
                ON agent_hire_requests(identity_id)
                WHERE status = 'pending';

            -- Event-derived retirement proposals. A proposal is an evidence
            -- bundle plus a decision, never an automatic action.
            CREATE TABLE IF NOT EXISTS agent_retirement_proposals (
                id               TEXT PRIMARY KEY,
                project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id      TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,
                status           TEXT NOT NULL,
                reason_code      TEXT NOT NULL,
                reasons_json     TEXT NOT NULL DEFAULT '[]',
                evidence_json    TEXT NOT NULL DEFAULT '[]',
                performance_json TEXT NOT NULL,
                proposed_at      TEXT NOT NULL,
                proposed_by      TEXT NOT NULL,
                decided_at       TEXT,
                decided_by       TEXT,
                decision_reason  TEXT,
                successor_id     TEXT,
                executed_at      TEXT,
                origin           TEXT NOT NULL DEFAULT 'local'
            );

            CREATE INDEX IF NOT EXISTS idx_agent_retirement_proposals_project
                ON agent_retirement_proposals(project_id, status, proposed_at);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_retirement_one_pending
                ON agent_retirement_proposals(identity_id)
                WHERE status = 'pending';

            -- Append-only employment stream. `identity_id` deliberately has no
            -- foreign key: a hire request can precede the identity row.
            CREATE TABLE IF NOT EXISTS agent_employment_events (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                seq         INTEGER NOT NULL,
                identity_id TEXT NOT NULL,
                kind        TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                UNIQUE (project_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_agent_employment_events_project
                ON agent_employment_events(project_id, seq);

            -- Deterministic per-identity performance ledger. This is derived
            -- read-only state: it can be rebuilt from task/usage/Verifier/
            -- diagnosis evidence and never authorizes a mutation.
            CREATE TABLE IF NOT EXISTS agent_performance (
                project_id          TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                identity_id         TEXT NOT NULL REFERENCES agent_identities(id) ON DELETE CASCADE,
                role                TEXT NOT NULL,
                status              TEXT NOT NULL,
                lookback_days       INTEGER NOT NULL,
                completed_tasks     INTEGER NOT NULL DEFAULT 0,
                failed_tasks        INTEGER NOT NULL DEFAULT 0,
                cancelled_tasks     INTEGER NOT NULL DEFAULT 0,
                blocked_tasks       INTEGER NOT NULL DEFAULT 0,
                skipped_tasks       INTEGER NOT NULL DEFAULT 0,
                consecutive_failures INTEGER NOT NULL DEFAULT 0,
                retries             INTEGER NOT NULL DEFAULT 0,
                verifier_approvals  INTEGER NOT NULL DEFAULT 0,
                verifier_rejections INTEGER NOT NULL DEFAULT 0,
                unsubstantiated_handoffs INTEGER NOT NULL DEFAULT 0,
                false_consensus     INTEGER NOT NULL DEFAULT 0,
                diagnosis_findings  INTEGER NOT NULL DEFAULT 0,
                assignment_count    INTEGER NOT NULL DEFAULT 0,
                session_count       INTEGER NOT NULL DEFAULT 0,
                input_tokens        INTEGER NOT NULL DEFAULT 0,
                output_tokens       INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens   INTEGER NOT NULL DEFAULT 0,
                estimated_usage_calls INTEGER NOT NULL DEFAULT 0,
                cost_usd            REAL NOT NULL DEFAULT 0,
                first_active_at     TEXT,
                last_active_at      TEXT,
                reason_counts_json  TEXT NOT NULL DEFAULT '[]',
                evidence_json       TEXT NOT NULL DEFAULT '[]',
                computed_at         TEXT NOT NULL,
                PRIMARY KEY (project_id, identity_id)
            );

            CREATE INDEX IF NOT EXISTS idx_agent_performance_project
                ON agent_performance(project_id, completed_tasks, failed_tasks);

            -- Portable team charters (S8). The JSON body is canonical,
            -- content-addressed, and contains no context/memory/inbox/
            -- worktree/performance evidence. `source_project_id` is local
            -- storage metadata, not part of the portable payload.
            CREATE TABLE IF NOT EXISTS team_templates (
                id                TEXT PRIMARY KEY,
                name              TEXT NOT NULL,
                version           TEXT NOT NULL,
                schema_version    INTEGER NOT NULL,
                content_hash      TEXT NOT NULL,
                json              TEXT NOT NULL,
                created_at        TEXT NOT NULL,
                source_project_id TEXT REFERENCES projects(id) ON DELETE SET NULL
            );

            CREATE INDEX IF NOT EXISTS idx_team_templates_content
                ON team_templates(content_hash);
            CREATE INDEX IF NOT EXISTS idx_team_templates_source
                ON team_templates(source_project_id, created_at);

            -- Explicit apply links. A template row without a link is a library
            -- record only: it creates no identity, hire request, session,
            -- assignment, workspace, memory, or policy change.
            CREATE TABLE IF NOT EXISTS project_team_templates (
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                template_id TEXT NOT NULL REFERENCES team_templates(id) ON DELETE CASCADE,
                applied_at  TEXT NOT NULL,
                applied_by  TEXT,
                origin      TEXT NOT NULL DEFAULT 'local',
                PRIMARY KEY (project_id, template_id)
            );

            CREATE INDEX IF NOT EXISTS idx_project_team_templates_project
                ON project_team_templates(project_id, applied_at);

            -- Project-scoped audit stream for export/import/apply. Payloads
            -- carry ids, names, hashes, and counts only; no template body and
            -- no context, memory, inbox, worktree, or secret content.
            CREATE TABLE IF NOT EXISTS team_template_events (
                id            TEXT PRIMARY KEY,
                project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                seq           INTEGER NOT NULL,
                kind          TEXT NOT NULL,
                template_id   TEXT NOT NULL,
                name          TEXT NOT NULL,
                version       TEXT NOT NULL,
                content_hash  TEXT NOT NULL,
                payload       TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                UNIQUE (project_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_team_template_events_project
                ON team_template_events(project_id, seq);
            CREATE INDEX IF NOT EXISTS idx_team_template_events_template
                ON team_template_events(template_id, seq);

            CREATE TABLE IF NOT EXISTS messages (
                id           TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role         TEXT NOT NULL,
                content      TEXT NOT NULL,
                tool_calls   TEXT NOT NULL DEFAULT '[]',
                tool_call_id TEXT,
                created_at   TEXT NOT NULL
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                tool_calls,
                content='messages',
                content_rowid='rowid',
                tokenize='unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS messages_fts_insert
            AFTER INSERT ON messages BEGIN
                INSERT INTO messages_fts(rowid, content, tool_calls)
                VALUES (new.rowid, new.content, new.tool_calls);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_delete
            AFTER DELETE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, tool_calls)
                VALUES ('delete', old.rowid, old.content, old.tool_calls);
            END;

            CREATE TRIGGER IF NOT EXISTS messages_fts_update
            AFTER UPDATE ON messages BEGIN
                INSERT INTO messages_fts(messages_fts, rowid, content, tool_calls)
                VALUES ('delete', old.rowid, old.content, old.tool_calls);
                INSERT INTO messages_fts(rowid, content, tool_calls)
                VALUES (new.rowid, new.content, new.tool_calls);
            END;

            CREATE VIRTUAL TABLE IF NOT EXISTS events_fts USING fts5(
                type,
                payload,
                content='events',
                content_rowid='rowid',
                tokenize='unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS events_fts_insert
            AFTER INSERT ON events BEGIN
                INSERT INTO events_fts(rowid, type, payload)
                VALUES (new.rowid, new.type, new.payload);
            END;

            CREATE TRIGGER IF NOT EXISTS events_fts_delete
            AFTER DELETE ON events BEGIN
                INSERT INTO events_fts(events_fts, rowid, type, payload)
                VALUES ('delete', old.rowid, old.type, old.payload);
            END;

            CREATE TRIGGER IF NOT EXISTS events_fts_update
            AFTER UPDATE ON events BEGIN
                INSERT INTO events_fts(events_fts, rowid, type, payload)
                VALUES ('delete', old.rowid, old.type, old.payload);
                INSERT INTO events_fts(rowid, type, payload)
                VALUES (new.rowid, new.type, new.payload);
            END;

            CREATE TABLE IF NOT EXISTS agent_messages (
                id            TEXT PRIMARY KEY,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                from_agent_id TEXT NOT NULL,
                from_name     TEXT NOT NULL,
                to_agent_id   TEXT NOT NULL,
                to_name       TEXT NOT NULL,
                kind          TEXT NOT NULL,
                body          TEXT NOT NULL,
                payload       TEXT NOT NULL DEFAULT '{}',
                status        TEXT NOT NULL,
                created_at    TEXT NOT NULL,
                read_at       TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_agent_messages_session
                ON agent_messages(session_id, created_at);

            CREATE TABLE IF NOT EXISTS artifacts (
                id         TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id   TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                task_id    TEXT,
                name       TEXT NOT NULL,
                kind       TEXT NOT NULL,
                mime_type  TEXT NOT NULL,
                content    TEXT NOT NULL,
                storage    TEXT NOT NULL DEFAULT 'inline',
                compression TEXT NOT NULL DEFAULT 'none',
                size_bytes INTEGER NOT NULL DEFAULT 0,
                sha256     TEXT NOT NULL DEFAULT '',
                path       TEXT,
                version    INTEGER NOT NULL DEFAULT 1,
                provenance TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS artifact_versions (
                artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
                version     INTEGER NOT NULL,
                name        TEXT NOT NULL,
                kind        TEXT NOT NULL,
                mime_type   TEXT NOT NULL,
                content     TEXT NOT NULL,
                storage     TEXT NOT NULL,
                compression TEXT NOT NULL,
                size_bytes  INTEGER NOT NULL,
                sha256      TEXT NOT NULL,
                path        TEXT,
                provenance TEXT NOT NULL DEFAULT '',
                created_at  TEXT NOT NULL,
                PRIMARY KEY (artifact_id, version)
            );

            CREATE INDEX IF NOT EXISTS idx_artifacts_session
                ON artifacts(session_id, created_at);

            CREATE TABLE IF NOT EXISTS artifact_requests (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                request_key TEXT NOT NULL,
                artifact_id TEXT NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
                created_at TEXT NOT NULL,
                PRIMARY KEY (session_id, request_key)
            );

            -- Full tool output is operational state, not a replay projection
            -- and not bundle content. The body lives in
            -- `<data_dir>/tool-outputs/<sha256>.txt`; this table is the
            -- daemon-owned handle registry.
            CREATE TABLE IF NOT EXISTS tool_outputs (
                id           TEXT PRIMARY KEY,
                project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                tool_call_id TEXT,
                tool_name    TEXT NOT NULL,
                sha256       TEXT NOT NULL,
                bytes        INTEGER NOT NULL,
                lines        INTEGER NOT NULL,
                created_at   TEXT NOT NULL,
                expires_at   TEXT,
                preview      TEXT NOT NULL DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_tool_outputs_project
                ON tool_outputs(project_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_tool_outputs_session
                ON tool_outputs(session_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_tool_outputs_expires
                ON tool_outputs(expires_at);

            CREATE TABLE IF NOT EXISTS usage_ledger (
                id            TEXT PRIMARY KEY,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id      TEXT NOT NULL,
                agent_name    TEXT NOT NULL,
                role          TEXT NOT NULL,
                task_id       TEXT,
                department    TEXT,
                model         TEXT NOT NULL,
                input_tokens  INTEGER NOT NULL,
                output_tokens INTEGER NOT NULL,
                cache_read_tokens  INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                estimated     INTEGER NOT NULL,
                cost_usd      REAL NOT NULL,
                created_at    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_usage_ledger_session
                ON usage_ledger(session_id, created_at);

            CREATE TABLE IF NOT EXISTS memories (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                kind        TEXT NOT NULL,
                title       TEXT NOT NULL,
                body        TEXT NOT NULL,
                tags        TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                status      TEXT NOT NULL DEFAULT 'active',
                source      TEXT NOT NULL DEFAULT 'agent',
                evidence    TEXT NOT NULL DEFAULT '[]',
                supersedes  TEXT NOT NULL DEFAULT '[]',
                superseded_by TEXT,
                pinned      INTEGER NOT NULL DEFAULT 0,
                use_count   INTEGER NOT NULL DEFAULT 0,
                last_used_at TEXT,
                last_verified_at TEXT,
                verify_after TEXT,
                stale_reason TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memories_project
                ON memories(project_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_session
                ON memories(session_id, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_project_status
                ON memories(project_id, status, created_at);

            CREATE INDEX IF NOT EXISTS idx_memories_project_kind_title
                ON memories(project_id, kind, title);

            CREATE TABLE IF NOT EXISTS memory_usage (
                memory_id  TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                session_id TEXT NOT NULL,
                used_at    TEXT NOT NULL,
                PRIMARY KEY(memory_id, session_id)
            );

            CREATE TABLE IF NOT EXISTS memory_conflicts (
                id               TEXT PRIMARY KEY,
                project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                memory_a_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                memory_b_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                memory_a_title   TEXT NOT NULL,
                memory_b_title   TEXT NOT NULL,
                memory_a_kind    TEXT NOT NULL,
                memory_b_kind    TEXT NOT NULL,
                memory_a_excerpt TEXT NOT NULL,
                memory_b_excerpt TEXT NOT NULL,
                scope_kind       TEXT NOT NULL,
                scope_value      TEXT NOT NULL,
                reason           TEXT NOT NULL,
                status           TEXT NOT NULL DEFAULT 'open',
                detected_at      TEXT NOT NULL,
                resolved_at      TEXT,
                resolution       TEXT,
                note             TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_memory_conflicts_project_status
                ON memory_conflicts(project_id, status, detected_at);

            CREATE TABLE IF NOT EXISTS memory_embeddings (
                memory_id    TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                model        TEXT NOT NULL,
                dimensions   INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                vector       BLOB NOT NULL,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embeddings_model
                ON memory_embeddings(model, dimensions);

            CREATE TABLE IF NOT EXISTS memory_embedding_jobs (
                memory_id       TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                model           TEXT NOT NULL,
                status          TEXT NOT NULL,
                attempts        INTEGER NOT NULL DEFAULT 0,
                last_error      TEXT,
                next_attempt_at TEXT,
                updated_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embedding_jobs_status
                ON memory_embedding_jobs(status, next_attempt_at, updated_at);

            CREATE TABLE IF NOT EXISTS memory_embedding_usage (
                id                 TEXT PRIMARY KEY,
                project_id         TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                model              TEXT NOT NULL,
                calls              INTEGER NOT NULL,
                input_tokens       INTEGER NOT NULL,
                estimated_tokens   INTEGER NOT NULL,
                bytes              INTEGER NOT NULL,
                provider_reported  INTEGER NOT NULL,
                created_at         TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embedding_usage_project
                ON memory_embedding_usage(project_id, model, created_at);

            CREATE TABLE IF NOT EXISTS memory_embedding_index (
                project_id           TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                model                TEXT NOT NULL,
                dimensions           INTEGER NOT NULL,
                generation           INTEGER NOT NULL,
                nlist                INTEGER NOT NULL,
                cluster_count        INTEGER NOT NULL,
                vector_count         INTEGER NOT NULL,
                centroids            BLOB NOT NULL,
                updated_at           TEXT NOT NULL,
                last_error           TEXT,
                fallback_count       INTEGER NOT NULL DEFAULT 0,
                last_fallback_at     TEXT,
                last_fallback_reason TEXT
            );

            CREATE TABLE IF NOT EXISTS memory_embedding_assignments (
                memory_id  TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                generation INTEGER NOT NULL,
                cluster    INTEGER NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memory_embedding_assignments_generation
                ON memory_embedding_assignments(generation, cluster);

            CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                title,
                body,
                tags,
                content='memories',
                content_rowid='rowid',
                tokenize='unicode61'
            );

            CREATE TRIGGER IF NOT EXISTS memories_fts_insert
            AFTER INSERT ON memories BEGIN
                INSERT INTO memories_fts(rowid, title, body, tags)
                VALUES (new.rowid, new.title, new.body, new.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_fts_delete
            AFTER DELETE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, body, tags)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags);
            END;

            CREATE TRIGGER IF NOT EXISTS memories_fts_update
            AFTER UPDATE ON memories BEGIN
                INSERT INTO memories_fts(memories_fts, rowid, title, body, tags)
                VALUES ('delete', old.rowid, old.title, old.body, old.tags);
                INSERT INTO memories_fts(rowid, title, body, tags)
                VALUES (new.rowid, new.title, new.body, new.tags);
            END;

            CREATE TABLE IF NOT EXISTS sources (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                collection  TEXT,
                title       TEXT NOT NULL,
                uri         TEXT,
                mime_type   TEXT,
                status      TEXT NOT NULL,
                size_bytes  INTEGER NOT NULL DEFAULT 0,
                sha256      TEXT NOT NULL,
                revision    INTEGER NOT NULL DEFAULT 1,
                chunk_count INTEGER NOT NULL DEFAULT 0,
                char_count  INTEGER NOT NULL DEFAULT 0,
                token_count INTEGER NOT NULL DEFAULT 0,
                error       TEXT,
                labels      TEXT NOT NULL DEFAULT '[]',
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                indexed_at  TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_sources_project_status
                ON sources(project_id, status, updated_at);

            CREATE TABLE IF NOT EXISTS source_chunks (
                id            TEXT PRIMARY KEY,
                project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                chunk_no      INTEGER NOT NULL,
                heading       TEXT,
                content       TEXT NOT NULL,
                start_offset  INTEGER NOT NULL DEFAULT 0,
                end_offset    INTEGER NOT NULL DEFAULT 0,
                token_count   INTEGER NOT NULL DEFAULT 0,
                created_at    TEXT NOT NULL,
                UNIQUE(source_id, chunk_no)
            );

            CREATE INDEX IF NOT EXISTS idx_source_chunks_project
                ON source_chunks(project_id, source_id, chunk_no);

            CREATE VIRTUAL TABLE IF NOT EXISTS source_chunks_fts USING fts5(
                content,
                heading,
                content='source_chunks',
                content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
            );

            CREATE TRIGGER IF NOT EXISTS source_chunks_fts_insert
            AFTER INSERT ON source_chunks BEGIN
                INSERT INTO source_chunks_fts(rowid, content, heading)
                VALUES (new.rowid, new.content, new.heading);
            END;

            CREATE TRIGGER IF NOT EXISTS source_chunks_fts_delete
            AFTER DELETE ON source_chunks BEGIN
                INSERT INTO source_chunks_fts(source_chunks_fts, rowid, content, heading)
                VALUES ('delete', old.rowid, old.content, old.heading);
            END;

            CREATE TRIGGER IF NOT EXISTS source_chunks_fts_update
            AFTER UPDATE ON source_chunks BEGIN
                INSERT INTO source_chunks_fts(source_chunks_fts, rowid, content, heading)
                VALUES ('delete', old.rowid, old.content, old.heading);
                INSERT INTO source_chunks_fts(rowid, content, heading)
                VALUES (new.rowid, new.content, new.heading);
            END;

            CREATE TABLE IF NOT EXISTS project_source_embedding_policies (
                project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy      TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            -- One current snapshot generation per source. Vectors from an older
            -- generation remain on disk as `stale` until retention pruning, but
            -- retrieval only joins the current generation.
            CREATE TABLE IF NOT EXISTS source_embedding_state (
                source_id   TEXT PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                generation  INTEGER NOT NULL DEFAULT 1,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_source_embedding_state_project
                ON source_embedding_state(project_id);

            CREATE TABLE IF NOT EXISTS source_embeddings (
                chunk_id      TEXT PRIMARY KEY,
                project_id    TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                source_id     TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                chunk_no      INTEGER NOT NULL,
                generation    INTEGER NOT NULL,
                model         TEXT NOT NULL,
                dimensions    INTEGER NOT NULL,
                content_hash  TEXT NOT NULL,
                vector        BLOB NOT NULL,
                status        TEXT NOT NULL DEFAULT 'current',
                created_at    TEXT NOT NULL,
                updated_at    TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_source_embeddings_project
                ON source_embeddings(project_id, source_id, generation);
            CREATE INDEX IF NOT EXISTS idx_source_embeddings_model
                ON source_embeddings(project_id, model, dimensions, status);

            CREATE TABLE IF NOT EXISTS source_embedding_jobs (
                source_id      TEXT PRIMARY KEY REFERENCES sources(id) ON DELETE CASCADE,
                project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                generation     INTEGER NOT NULL,
                model          TEXT NOT NULL,
                status         TEXT NOT NULL,
                attempts       INTEGER NOT NULL DEFAULT 0,
                chunks_total   INTEGER NOT NULL DEFAULT 0,
                chunks_done    INTEGER NOT NULL DEFAULT 0,
                last_error     TEXT,
                next_attempt_at TEXT,
                updated_at     TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_source_embedding_jobs_status
                ON source_embedding_jobs(project_id, status, next_attempt_at, updated_at);

            CREATE TABLE IF NOT EXISTS source_embedding_usage (
                id                    TEXT PRIMARY KEY,
                project_id            TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                model                 TEXT NOT NULL,
                calls                 INTEGER NOT NULL DEFAULT 0,
                input_tokens          INTEGER NOT NULL DEFAULT 0,
                estimated_tokens      INTEGER NOT NULL DEFAULT 0,
                bytes                 INTEGER NOT NULL DEFAULT 0,
                provider_reported     INTEGER NOT NULL DEFAULT 0,
                created_at            TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_source_embedding_usage_project
                ON source_embedding_usage(project_id, model, created_at);

            -- Project-scoped retrieval v2 controls. Additive policy, stored as
            -- JSON so older binaries decode the documented defaults.
            CREATE TABLE IF NOT EXISTS project_source_retrieval_policies (
                project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy      TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            -- Derived source IVF index. One row per project, scoped by
            -- model/dimensions/generation. Centroids are versioned bytes and
            -- never leave the daemon.
            CREATE TABLE IF NOT EXISTS source_ann_index (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                model          TEXT NOT NULL,
                dimensions     INTEGER NOT NULL,
                nlist          INTEGER NOT NULL,
                centroid_count INTEGER NOT NULL,
                vector_count   INTEGER NOT NULL,
                generation     INTEGER NOT NULL,
                centroids      BLOB NOT NULL,
                last_error     TEXT,
                created_at     TEXT NOT NULL,
                updated_at     TEXT NOT NULL
            );

            -- Cluster assignment for one stored chunk. `generation` is the
            -- index generation, not the source snapshot generation.
            CREATE TABLE IF NOT EXISTS source_ann_assignments (
                chunk_id    TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                model       TEXT NOT NULL,
                dimensions  INTEGER NOT NULL,
                generation  INTEGER NOT NULL,
                cluster     INTEGER NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_source_ann_assignments_project
                ON source_ann_assignments(project_id, generation, cluster);
            CREATE INDEX IF NOT EXISTS idx_source_ann_assignments_model
                ON source_ann_assignments(project_id, model, dimensions, generation);

            CREATE TABLE IF NOT EXISTS skills (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name        TEXT NOT NULL,
                slug        TEXT NOT NULL,
                description TEXT NOT NULL,
                when_to_use TEXT NOT NULL DEFAULT '',
                version     TEXT NOT NULL DEFAULT '1.0.0',
                instructions TEXT NOT NULL,
                tool_allow  TEXT NOT NULL DEFAULT '[]',
                tool_deny   TEXT NOT NULL DEFAULT '[]',
                profiles    TEXT NOT NULL DEFAULT '[]',
                roles       TEXT NOT NULL DEFAULT '[]',
                enabled     INTEGER NOT NULL DEFAULT 1,
                source      TEXT NOT NULL DEFAULT 'user',
                sha256      TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                UNIQUE(project_id, slug)
            );

            CREATE INDEX IF NOT EXISTS idx_skills_project_enabled
                ON skills(project_id, enabled, updated_at);

            CREATE TABLE IF NOT EXISTS approval_grants (
                id               TEXT PRIMARY KEY,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                tool              TEXT NOT NULL,
                path_prefix       TEXT NOT NULL,
                include_children  INTEGER NOT NULL DEFAULT 0,
                created_by        TEXT NOT NULL,
                created_at        TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_approval_grants_project
                ON approval_grants(project_id, created_at);

            CREATE TABLE IF NOT EXISTS approval_templates (
                id             TEXT PRIMARY KEY,
                project_id     TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                name           TEXT NOT NULL,
                enabled        INTEGER NOT NULL DEFAULT 1,
                tool_pattern   TEXT NOT NULL,
                risk_levels    TEXT NOT NULL DEFAULT '[]',
                path_globs     TEXT NOT NULL DEFAULT '[]',
                action         TEXT NOT NULL DEFAULT 'allow',
                expires_at     TEXT,
                max_uses       INTEGER,
                used_count     INTEGER NOT NULL DEFAULT 0,
                created_by     TEXT NOT NULL,
                created_at     TEXT NOT NULL,
                updated_at     TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_approval_templates_project
                ON approval_templates(project_id, enabled, created_at);

            CREATE TABLE IF NOT EXISTS project_attention_policies (
                project_id  TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy      TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS approval_audit (
                id          TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                kind        TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_approval_audit_project
                ON approval_audit(project_id, created_at);

            CREATE TABLE IF NOT EXISTS questions (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                question    TEXT NOT NULL,
                options     TEXT NOT NULL,
                status      TEXT NOT NULL,
                answer      TEXT,
                created_at  TEXT NOT NULL,
                answered_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_questions_session
                ON questions(session_id, created_at);

            CREATE TABLE IF NOT EXISTS progress_rounds (
                seq             INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_revision   INTEGER NOT NULL,
                round_id        TEXT NOT NULL,
                agent_id        TEXT,
                agent_name      TEXT,
                task_id         TEXT,
                satisfied_json  TEXT NOT NULL,
                in_loop_json    TEXT NOT NULL,
                progressing_json TEXT NOT NULL,
                blockers_json   TEXT NOT NULL,
                next_task       TEXT,
                instruction     TEXT,
                evidence_json   TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                UNIQUE (session_id, plan_revision, round_id)
            );

            CREATE INDEX IF NOT EXISTS idx_progress_rounds_session
                ON progress_rounds(session_id, plan_revision, seq);

            CREATE TABLE IF NOT EXISTS coordination_notes (
                note_id          TEXT PRIMARY KEY,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq              INTEGER NOT NULL,
                action           TEXT NOT NULL,
                severity         TEXT NOT NULL,
                status           TEXT NOT NULL,
                task_id          TEXT,
                target_agent_id  TEXT,
                agent_id         TEXT NOT NULL,
                agent_name       TEXT NOT NULL,
                message          TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                resolved_at      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_coordination_notes_session
                ON coordination_notes(session_id, seq);

            CREATE TABLE IF NOT EXISTS orchestration_decisions (
                decision_id      TEXT PRIMARY KEY,
                session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq              INTEGER NOT NULL,
                action           TEXT NOT NULL,
                task_id          TEXT,
                detail           TEXT NOT NULL,
                attempt          INTEGER NOT NULL DEFAULT 0,
                agent_id         TEXT NOT NULL,
                created_at       TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_orchestration_decisions_session
                ON orchestration_decisions(session_id, seq);

            CREATE TABLE IF NOT EXISTS mcp_servers (
                id          TEXT PRIMARY KEY,
                name        TEXT NOT NULL UNIQUE,
                command     TEXT NOT NULL,
                args        TEXT NOT NULL,
                env         TEXT NOT NULL,
                enabled     INTEGER NOT NULL DEFAULT 1,
                created_at  TEXT NOT NULL,
                transport   TEXT NOT NULL DEFAULT 'stdio',
                url         TEXT,
                headers     TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS acp_agents (
                id                   TEXT PRIMARY KEY,
                name                 TEXT NOT NULL UNIQUE,
                command              TEXT NOT NULL,
                args                 TEXT NOT NULL,
                env                  TEXT NOT NULL,
                enabled              INTEGER NOT NULL DEFAULT 1,
                auto_approve         INTEGER NOT NULL DEFAULT 0,
                timeout_secs         INTEGER NOT NULL DEFAULT 1800,
                created_at           TEXT NOT NULL,
                transport            TEXT NOT NULL DEFAULT 'stdio',
                endpoint             TEXT,
                auth_header_env      TEXT,
                headers              TEXT NOT NULL DEFAULT '{}',
                connect_timeout_secs INTEGER NOT NULL DEFAULT 30,
                allow_remote         INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS project_memory_policies (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled        INTEGER NOT NULL,
                max_items      INTEGER,
                max_age_days   INTEGER,
                embedding_json TEXT,
                updated_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_worktree_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                older_than_secs  INTEGER NOT NULL DEFAULT 604800,
                delete_branches  INTEGER NOT NULL DEFAULT 0,
                keep_failed      INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS project_checkpoint_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                older_than_secs  INTEGER NOT NULL DEFAULT 2592000,
                keep_latest      INTEGER NOT NULL DEFAULT 5
            );

            CREATE TABLE IF NOT EXISTS project_artifact_policies (
                project_id   TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                auto_gc      INTEGER NOT NULL,
                max_bytes    INTEGER,
                max_age_secs INTEGER,
                max_count    INTEGER,
                updated_at   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_tool_output_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Test-result reuse policy (ADR-0178). Stored as JSON so the
            -- policy can grow additively; reads normalize and clamp bounds.
            CREATE TABLE IF NOT EXISTS project_test_cache_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Content-addressed test/lint/check results. `cache_key` is a
            -- versioned digest over command, argv, cwd, normalized environment,
            -- profile, tool version, input content, and impact generation.
            -- `result_json` is a bounded structured payload used for cache-hit
            -- rendering; full command output lives in the tool-output store as
            -- an opaque `output_ref`, never inline here.
            CREATE TABLE IF NOT EXISTS test_results (
                id                TEXT PRIMARY KEY,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                kind              TEXT NOT NULL,
                command           TEXT NOT NULL,
                command_source    TEXT NOT NULL DEFAULT '',
                key_version       INTEGER NOT NULL DEFAULT 1,
                cache_key         TEXT NOT NULL,
                inputs_hash       TEXT NOT NULL,
                impact_generation TEXT NOT NULL,
                status            TEXT NOT NULL,
                exit_code         INTEGER NOT NULL,
                duration_ms       INTEGER NOT NULL,
                output_ref        TEXT,
                output_bytes      INTEGER NOT NULL DEFAULT 0,
                covered_paths     TEXT NOT NULL DEFAULT '[]',
                warnings          TEXT NOT NULL DEFAULT '[]',
                warning_count     INTEGER NOT NULL DEFAULT 0,
                failure_count     INTEGER NOT NULL DEFAULT 0,
                result_json       TEXT NOT NULL DEFAULT '{}',
                created_at        TEXT NOT NULL,
                expires_at        TEXT NOT NULL,
                hit_count         INTEGER NOT NULL DEFAULT 0,
                last_hit_at       TEXT,
                UNIQUE(project_id, cache_key)
            );

            CREATE INDEX IF NOT EXISTS idx_test_results_project_created
                ON test_results(project_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_test_results_project_expires
                ON test_results(project_id, expires_at);
            CREATE INDEX IF NOT EXISTS idx_test_results_session
                ON test_results(session_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_test_results_project_kind
                ON test_results(project_id, kind, status, created_at DESC);

            CREATE TABLE IF NOT EXISTS project_budgets (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                token_budget   INTEGER,
                cost_budget_usd REAL,
                updated_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_task_scheduler_policies (
                project_id          TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled             INTEGER NOT NULL DEFAULT 1,
                cooldown_secs       INTEGER NOT NULL DEFAULT 60,
                dispatch_tasks      INTEGER NOT NULL DEFAULT 0,
                include_intake      INTEGER NOT NULL DEFAULT 1,
                max_intake_parallel INTEGER NOT NULL DEFAULT 2,
                updated_at          TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_scaling_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Event-backed deterministic scaling decisions. `decision_key` is
            -- the plan-revision/tick idempotency key, unique per session.
            CREATE TABLE IF NOT EXISTS scaling_decisions (
                decision_id          TEXT PRIMARY KEY,
                session_id           TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id           TEXT NOT NULL,
                seq                  INTEGER NOT NULL DEFAULT 0,
                decision_key         TEXT NOT NULL,
                plan_revision        INTEGER NOT NULL DEFAULT 0,
                action               TEXT NOT NULL,
                current_workers      INTEGER NOT NULL DEFAULT 0,
                current_labour       INTEGER NOT NULL DEFAULT 0,
                target_workers       INTEGER NOT NULL DEFAULT 0,
                target_labour        INTEGER NOT NULL DEFAULT 0,
                active_workers       INTEGER NOT NULL DEFAULT 0,
                active_labour        INTEGER NOT NULL DEFAULT 0,
                ready_tasks          INTEGER NOT NULL DEFAULT 0,
                running_tasks        INTEGER NOT NULL DEFAULT 0,
                inputs_hash          TEXT NOT NULL,
                reasons              TEXT NOT NULL DEFAULT '[]',
                manager_recommended  INTEGER NOT NULL DEFAULT 0,
                manager_evidence     TEXT,
                applied              INTEGER NOT NULL DEFAULT 0,
                idle_since_workers   TEXT,
                idle_since_labour    TEXT,
                replay_inputs_json   TEXT,
                created_at           TEXT NOT NULL,
                UNIQUE(session_id, decision_key)
            );

            CREATE INDEX IF NOT EXISTS idx_scaling_decisions_session
                ON scaling_decisions(session_id, seq);
            CREATE INDEX IF NOT EXISTS idx_scaling_decisions_project
                ON scaling_decisions(project_id, created_at);

            CREATE TABLE IF NOT EXISTS intake_tasks (
                id              TEXT PRIMARY KEY,
                project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                title           TEXT NOT NULL,
                instructions    TEXT NOT NULL,
                profile         TEXT NOT NULL,
                kind            TEXT NOT NULL,
                priority        INTEGER NOT NULL DEFAULT 0,
                write_scope     TEXT NOT NULL DEFAULT '[]',
                acceptance      TEXT NOT NULL DEFAULT '[]',
                deps            TEXT NOT NULL DEFAULT '[]',
                token_budget    INTEGER,
                cost_budget_usd REAL,
                time_budget_secs INTEGER,
                status          TEXT NOT NULL,
                attempts        INTEGER NOT NULL DEFAULT 0,
                max_attempts    INTEGER NOT NULL DEFAULT 2,
                next_attempt_at TEXT,
                session_id      TEXT,
                task_id         TEXT,
                last_error      TEXT,
                result_summary  TEXT,
                origin          TEXT NOT NULL DEFAULT 'user',
                labels          TEXT NOT NULL DEFAULT '[]',
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_intake_tasks_project_status
                ON intake_tasks(project_id, status, created_at);
            CREATE INDEX IF NOT EXISTS idx_intake_tasks_runnable
                ON intake_tasks(status, next_attempt_at);

            CREATE TABLE IF NOT EXISTS programs (
                id              TEXT PRIMARY KEY,
                project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                goal            TEXT NOT NULL,
                status          TEXT NOT NULL,
                policy          TEXT NOT NULL DEFAULT '{}',
                token_budget    INTEGER,
                cost_budget_usd REAL,
                reason          TEXT,
                archived        INTEGER NOT NULL DEFAULT 0,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                completed_at    TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_programs_project_status
                ON programs(project_id, status, created_at);

            CREATE TABLE IF NOT EXISTS program_milestones (
                id              TEXT PRIMARY KEY,
                program_id      TEXT NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
                key             TEXT NOT NULL,
                title           TEXT NOT NULL,
                objective       TEXT NOT NULL,
                acceptance      TEXT NOT NULL DEFAULT '[]',
                deps            TEXT NOT NULL DEFAULT '[]',
                priority        INTEGER NOT NULL DEFAULT 0,
                status          TEXT NOT NULL,
                session_id      TEXT,
                plan_revision   INTEGER,
                attempts        INTEGER NOT NULL DEFAULT 0,
                max_attempts    INTEGER NOT NULL DEFAULT 2,
                next_attempt_at TEXT,
                result          TEXT,
                last_error      TEXT,
                profile         TEXT NOT NULL DEFAULT 'coding',
                write_scope     TEXT NOT NULL DEFAULT '[]',
                token_budget    INTEGER,
                cost_budget_usd REAL,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                completed_at    TEXT,
                UNIQUE(program_id, key)
            );

            CREATE INDEX IF NOT EXISTS idx_program_milestones_program_status
                ON program_milestones(program_id, status, next_attempt_at);
            CREATE INDEX IF NOT EXISTS idx_program_milestones_session
                ON program_milestones(session_id);

            CREATE TABLE IF NOT EXISTS program_events (
                project_id  TEXT NOT NULL,
                program_id  TEXT NOT NULL,
                seq         INTEGER NOT NULL,
                kind        TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                PRIMARY KEY (program_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_program_events_project
                ON program_events(project_id, created_at);

            -- Append-only accounting ledger for every milestone dispatch.
            -- `program_milestones.session_id` only points at the current
            -- attempt; retries clear it. Program budgets must still count the
            -- spend of every earlier attempt, so links are never deleted on
            -- retry and are rebuilt from `program.milestone.updated` events.
            CREATE TABLE IF NOT EXISTS program_milestone_sessions (
                program_id    TEXT NOT NULL REFERENCES programs(id) ON DELETE CASCADE,
                milestone_id  TEXT NOT NULL REFERENCES program_milestones(id) ON DELETE CASCADE,
                session_id    TEXT NOT NULL,
                attempt       INTEGER NOT NULL,
                dispatched_at TEXT NOT NULL,
                PRIMARY KEY (milestone_id, session_id)
            );

            CREATE INDEX IF NOT EXISTS idx_program_milestone_sessions_program
                ON program_milestone_sessions(program_id, session_id);
            CREATE INDEX IF NOT EXISTS idx_program_milestone_sessions_session
                ON program_milestone_sessions(session_id);

            CREATE TABLE IF NOT EXISTS project_orchestration_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_critical_path_policies (
                project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                policy     TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_mailbox_policies (
                project_id    TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                max_age_secs  INTEGER,
                max_count     INTEGER,
                delete_unread INTEGER NOT NULL DEFAULT 0,
                auto_gc       INTEGER NOT NULL DEFAULT 0,
                updated_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS approvals (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                tool_call_id    TEXT NOT NULL,
                tool            TEXT NOT NULL,
                input           TEXT NOT NULL,
                status          TEXT NOT NULL,
                created_at      TEXT NOT NULL,
                resolved_at     TEXT,
                approval_risk   TEXT,
                approval_reason TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id, created_at);
            CREATE TABLE IF NOT EXISTS todos (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                position   INTEGER NOT NULL,
                title      TEXT NOT NULL,
                status     TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (session_id, position)
            );

            CREATE INDEX IF NOT EXISTS idx_approvals_session
                ON approvals(session_id, created_at);

            CREATE TABLE IF NOT EXISTS steering_messages (
                id           TEXT PRIMARY KEY,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id     TEXT NOT NULL,
                task_id      TEXT,
                body         TEXT NOT NULL,
                status       TEXT NOT NULL,
                created_at   TEXT NOT NULL,
                delivered_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_steering_messages_session
                ON steering_messages(session_id, agent_id, status, created_at);

            CREATE TABLE IF NOT EXISTS session_operations (
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                operation_id TEXT NOT NULL,
                method       TEXT NOT NULL,
                fingerprint  TEXT NOT NULL,
                status       TEXT NOT NULL,
                result_json  TEXT,
                error        TEXT,
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (session_id, operation_id)
            );

            CREATE INDEX IF NOT EXISTS idx_session_operations_status
                ON session_operations(status, updated_at);

            CREATE TABLE IF NOT EXISTS project_session_run_policies (
                project_id     TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                auto_resume    INTEGER NOT NULL DEFAULT 0,
                max_attempts   INTEGER NOT NULL DEFAULT 1,
                max_age_hours  INTEGER NOT NULL DEFAULT 24,
                updated_at     TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS session_runs (
                run_id                 TEXT PRIMARY KEY,
                session_id             TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id             TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                agent_id               TEXT NOT NULL,
                agent_name             TEXT NOT NULL DEFAULT '',
                agent_role             TEXT NOT NULL DEFAULT '',
                task_id                TEXT,
                department             TEXT,
                objective              TEXT NOT NULL DEFAULT '',
                provider               TEXT NOT NULL DEFAULT '',
                model                  TEXT NOT NULL DEFAULT '',
                status                 TEXT NOT NULL,
                attempt                INTEGER NOT NULL DEFAULT 1,
                resumed_from_run_id    TEXT,
                resume_root_id         TEXT NOT NULL DEFAULT '',
                top_level              INTEGER NOT NULL DEFAULT 0,
                started_at             TEXT NOT NULL,
                updated_at             TEXT NOT NULL,
                ended_at               TEXT,
                budget_json            TEXT NOT NULL DEFAULT '{}',
                worktree_head          TEXT NOT NULL DEFAULT '',
                plan_revision          INTEGER NOT NULL DEFAULT 0,
                runtime_snapshot_json  TEXT,
                last_event_seq         INTEGER NOT NULL DEFAULT 0,
                failure_json           TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_session_runs_session
                ON session_runs(session_id, started_at DESC);
            CREATE INDEX IF NOT EXISTS idx_session_runs_status
                ON session_runs(status, started_at);
            CREATE INDEX IF NOT EXISTS idx_session_runs_root
                ON session_runs(resume_root_id, started_at);
            CREATE INDEX IF NOT EXISTS idx_session_runs_resumable
                ON session_runs(top_level, status, started_at);

            CREATE TABLE IF NOT EXISTS run_steps (
                step_id      TEXT PRIMARY KEY,
                run_id       TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq          INTEGER NOT NULL,
                turn         INTEGER NOT NULL DEFAULT 0,
                kind         TEXT NOT NULL,
                status       TEXT NOT NULL,
                input_hash   TEXT NOT NULL DEFAULT '',
                output_hash  TEXT NOT NULL DEFAULT '',
                payload_json TEXT NOT NULL DEFAULT '{}',
                created_at   TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                UNIQUE (run_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_run_steps_run
                ON run_steps(run_id, seq);
            CREATE INDEX IF NOT EXISTS idx_run_steps_session
                ON run_steps(session_id, created_at);

            CREATE TABLE IF NOT EXISTS run_tool_calls (
                run_id             TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                session_id         TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                step_id            TEXT NOT NULL,
                tool_call_id       TEXT NOT NULL,
                tool_name          TEXT NOT NULL,
                input_fingerprint  TEXT NOT NULL,
                status             TEXT NOT NULL,
                result_json        TEXT,
                error              TEXT,
                consumed_by_run_id TEXT,
                consumed_at        TEXT,
                created_at         TEXT NOT NULL,
                updated_at         TEXT NOT NULL,
                PRIMARY KEY (run_id, tool_call_id)
            );

            CREATE INDEX IF NOT EXISTS idx_run_tool_calls_session
                ON run_tool_calls(session_id, created_at);
            CREATE INDEX IF NOT EXISTS idx_run_tool_calls_match
                ON run_tool_calls(tool_name, input_fingerprint, status);
            CREATE INDEX IF NOT EXISTS idx_run_tool_calls_step
                ON run_tool_calls(run_id, step_id);

            CREATE TABLE IF NOT EXISTS run_checkpoints (
                checkpoint_id  TEXT PRIMARY KEY,
                run_id         TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE CASCADE,
                session_id     TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq            INTEGER NOT NULL,
                turn           INTEGER NOT NULL DEFAULT 0,
                message_cursor INTEGER NOT NULL DEFAULT 0,
                event_cursor   INTEGER NOT NULL DEFAULT 0,
                budget_json    TEXT NOT NULL DEFAULT '{}',
                worktree_head  TEXT NOT NULL DEFAULT '',
                plan_revision  INTEGER NOT NULL DEFAULT 0,
                created_at     TEXT NOT NULL,
                UNIQUE (run_id, seq)
            );

            CREATE INDEX IF NOT EXISTS idx_run_checkpoints_run
                ON run_checkpoints(run_id, seq);

            -- Durable active-context boundaries. The full checkpoint is also
            -- carried by context.checkpointed events, so this table is an
            -- event-backed projection that replay/repair can rebuild.
            CREATE TABLE IF NOT EXISTS context_checkpoints (
                id                  TEXT PRIMARY KEY,
                session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id          TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                label               TEXT NOT NULL DEFAULT '',
                mode                TEXT NOT NULL DEFAULT 'manual',
                summary             TEXT,
                through_message_id  TEXT,
                message_cursor      INTEGER NOT NULL DEFAULT 0,
                plan_revision       INTEGER NOT NULL DEFAULT 0,
                provider            TEXT NOT NULL DEFAULT '',
                model               TEXT NOT NULL DEFAULT '',
                prompt_prefix_hash  TEXT NOT NULL DEFAULT '',
                worktree_head       TEXT NOT NULL DEFAULT '',
                tokens_before       INTEGER NOT NULL DEFAULT 0,
                tokens_after        INTEGER NOT NULL DEFAULT 0,
                structured_state    TEXT,
                source_compaction_seq INTEGER,
                automatic           INTEGER NOT NULL DEFAULT 0,
                created_at          TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_context_checkpoints_session
                ON context_checkpoints(session_id, created_at);

            CREATE TABLE IF NOT EXISTS project_context_checkpoint_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                max_checkpoints  INTEGER NOT NULL DEFAULT 20,
                max_age_days     INTEGER NOT NULL DEFAULT 30
            );

            -- Durable supervised processes. Lifecycle transitions are mirrored
            -- to process.* events; heartbeat, last-access, and output counters
            -- are operational observation state updated without events.
            CREATE TABLE IF NOT EXISTS supervised_processes (
                id                  TEXT PRIMARY KEY,
                session_id          TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                name                TEXT NOT NULL DEFAULT '',
                command             TEXT NOT NULL,
                argv                TEXT NOT NULL DEFAULT '[]',
                cwd                 TEXT NOT NULL DEFAULT '',
                worktree            TEXT NOT NULL,
                env_hash            TEXT NOT NULL DEFAULT '',
                command_hash        TEXT NOT NULL DEFAULT '',
                state               TEXT NOT NULL,
                pid                 INTEGER,
                pgid                INTEGER,
                nonce               TEXT NOT NULL DEFAULT '',
                started_at          TEXT NOT NULL,
                process_started_at  TEXT,
                heartbeat_at        TEXT NOT NULL,
                finished_at         TEXT,
                exit_code           INTEGER,
                signal              INTEGER,
                output_path         TEXT NOT NULL DEFAULT '',
                output_bytes        INTEGER NOT NULL DEFAULT 0,
                output_lines        INTEGER NOT NULL DEFAULT 0,
                total_output_bytes  INTEGER NOT NULL DEFAULT 0,
                output_rotations    INTEGER NOT NULL DEFAULT 0,
                max_output_bytes    INTEGER NOT NULL DEFAULT 1048576,
                max_output_lines    INTEGER NOT NULL DEFAULT 20000,
                owner_instance      TEXT NOT NULL DEFAULT '',
                retention_deadline  TEXT,
                idle_ttl_secs       INTEGER NOT NULL DEFAULT 3600,
                last_access_at      TEXT NOT NULL,
                restart_attempts    INTEGER NOT NULL DEFAULT 0,
                restart_blocked_reason TEXT,
                detail              TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_supervised_processes_session
                ON supervised_processes(session_id, started_at);
            CREATE INDEX IF NOT EXISTS idx_supervised_processes_state
                ON supervised_processes(state, heartbeat_at);
            CREATE INDEX IF NOT EXISTS idx_supervised_processes_retention
                ON supervised_processes(retention_deadline);

            CREATE TABLE IF NOT EXISTS session_continuations (
                id              TEXT PRIMARY KEY,
                session_id      TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                body            TEXT NOT NULL,
                watch_task_ids  TEXT NOT NULL DEFAULT '[]',
                watch_mode      TEXT NOT NULL DEFAULT 'all',
                deadline_at     TEXT,
                status          TEXT NOT NULL,
                attempts        INTEGER NOT NULL DEFAULT 0,
                max_attempts    INTEGER NOT NULL DEFAULT 3,
                wake_reason     TEXT,
                outcomes        TEXT NOT NULL DEFAULT '[]',
                last_error      TEXT,
                next_attempt_at TEXT,
                created_at      TEXT NOT NULL,
                ready_at        TEXT,
                delivered_at    TEXT,
                updated_at      TEXT NOT NULL,
                series_id       TEXT NOT NULL DEFAULT '',
                parent_continuation_id TEXT,
                recurrence_every_secs INTEGER,
                recurrence_max_runs INTEGER NOT NULL DEFAULT 1,
                run_count       INTEGER NOT NULL DEFAULT 0,
                lease_owner     TEXT,
                lease_token     TEXT,
                lease_expires_at TEXT,
                delivery_attempt INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_session_continuations_status
                ON session_continuations(status, next_attempt_at, deadline_at);
            CREATE INDEX IF NOT EXISTS idx_session_continuations_session
                ON session_continuations(session_id, status, created_at);

            CREATE TABLE IF NOT EXISTS merge_queue (
                id          TEXT PRIMARY KEY,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id    TEXT NOT NULL,
                agent_name  TEXT NOT NULL,
                task_id     TEXT,
                branch       TEXT NOT NULL,
                commit_hash  TEXT NOT NULL,
                base_commit  TEXT NOT NULL,
                pre_merge_commit TEXT,
                status      TEXT NOT NULL,
                attempt     INTEGER NOT NULL DEFAULT 1,
                detail      TEXT,
                source      TEXT NOT NULL DEFAULT 'local',
                remote_host TEXT,
                remote_branch TEXT,
                bundle_sha256 TEXT,
                imported_ref TEXT,
                verifier_approved INTEGER,
                verifier_summary TEXT,
                environment_caveat TEXT,
                remote_test_evidence_id TEXT,
                remote_test_digest TEXT,
                remote_test_status TEXT,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_merge_queue_session
                ON merge_queue(session_id, created_at);

            CREATE TABLE IF NOT EXISTS project_remote_integration_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                max_bundle_bytes INTEGER NOT NULL,
                require_verifier INTEGER NOT NULL DEFAULT 1,
                allowed_hosts    TEXT NOT NULL DEFAULT '[]',
                keep_import_refs INTEGER NOT NULL DEFAULT 0,
                updated_at       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_remote_test_policies (
                project_id       TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                enabled          INTEGER NOT NULL DEFAULT 0,
                default_profile  TEXT,
                profiles         TEXT NOT NULL DEFAULT '[]',
                timeout_secs     INTEGER NOT NULL,
                output_cap_bytes INTEGER NOT NULL,
                updated_at       TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS remote_bootstrap_records (
                project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                host             TEXT NOT NULL,
                install_path     TEXT NOT NULL,
                sha256           TEXT NOT NULL,
                bytes            INTEGER NOT NULL,
                helper_version   TEXT NOT NULL,
                protocol_version INTEGER NOT NULL,
                target_os        TEXT NOT NULL,
                target_arch      TEXT NOT NULL,
                installed_at     TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                PRIMARY KEY (project_id, host, install_path)
            );

            CREATE TABLE IF NOT EXISTS remote_test_evidence (
                id                TEXT PRIMARY KEY,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                entry_id          TEXT,
                profile           TEXT NOT NULL,
                kind              TEXT NOT NULL,
                host              TEXT NOT NULL,
                commit_hash       TEXT NOT NULL,
                command           TEXT NOT NULL,
                cwd               TEXT,
                status            TEXT NOT NULL,
                exit_code         INTEGER,
                duration_ms       INTEGER NOT NULL,
                output_ref        TEXT,
                output_sha256     TEXT,
                output_bytes      INTEGER NOT NULL DEFAULT 0,
                output_truncated  INTEGER NOT NULL DEFAULT 0,
                timed_out         INTEGER NOT NULL DEFAULT 0,
                detail            TEXT,
                digest            TEXT NOT NULL,
                environment_caveat TEXT NOT NULL,
                created_at        TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_remote_test_evidence_project
                ON remote_test_evidence(project_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_remote_test_evidence_session
                ON remote_test_evidence(session_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_remote_test_evidence_entry
                ON remote_test_evidence(entry_id);

            CREATE TABLE IF NOT EXISTS merge_auto_attempts (
                entry_id TEXT PRIMARY KEY REFERENCES merge_queue(id) ON DELETE CASCADE,
                attempts INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS plan_tasks (
                session_id   TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_seq     INTEGER NOT NULL,
                task_id      TEXT NOT NULL,
                project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                department   TEXT NOT NULL,
                title        TEXT NOT NULL,
                priority     INTEGER NOT NULL DEFAULT 0,
                status       TEXT NOT NULL,
                deps         TEXT NOT NULL DEFAULT '[]',
                write_scope  TEXT NOT NULL DEFAULT '[]',
                attempts     INTEGER NOT NULL DEFAULT 0,
                last_error   TEXT,
                next_attempt_at TEXT,
                enqueued_at  TEXT NOT NULL,
                started_at   TEXT,
                finished_at  TEXT,
                updated_at   TEXT NOT NULL,
                PRIMARY KEY (session_id, plan_seq, task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_plan_tasks_queue
                ON plan_tasks(status, priority, enqueued_at);
            CREATE INDEX IF NOT EXISTS idx_plan_tasks_session
                ON plan_tasks(session_id, plan_seq);

            CREATE TABLE IF NOT EXISTS plan_task_progress (
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_seq          INTEGER NOT NULL,
                task_id           TEXT NOT NULL,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                last_progress_at  TEXT NOT NULL,
                stall_count       INTEGER NOT NULL DEFAULT 0,
                last_turn         INTEGER,
                last_tool         TEXT,
                updated_at        TEXT NOT NULL,
                PRIMARY KEY (session_id, plan_seq, task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_plan_task_progress_session
                ON plan_task_progress(session_id, task_id);

            CREATE TABLE IF NOT EXISTS orchestration_recoveries (
                session_id        TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                plan_seq          INTEGER NOT NULL,
                task_id           TEXT NOT NULL,
                project_id        TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                department        TEXT NOT NULL,
                action            TEXT NOT NULL,
                status            TEXT NOT NULL,
                attempts          INTEGER NOT NULL DEFAULT 0,
                last_stall_count  INTEGER NOT NULL DEFAULT 0,
                requested_at      TEXT NOT NULL,
                updated_at        TEXT NOT NULL,
                last_error        TEXT,
                next_action_at    TEXT,
                PRIMARY KEY (session_id, plan_seq, task_id)
            );

            CREATE INDEX IF NOT EXISTS idx_orchestration_recoveries_session
                ON orchestration_recoveries(session_id, plan_seq, task_id);
            CREATE INDEX IF NOT EXISTS idx_orchestration_recoveries_status
                ON orchestration_recoveries(status, updated_at);

            CREATE TABLE IF NOT EXISTS write_leases (
                agent_id    TEXT PRIMARY KEY,
                project_id  TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                scopes      TEXT NOT NULL DEFAULT '[]',
                acquired_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_write_leases_project
                ON write_leases(project_id, acquired_at);

            CREATE TABLE IF NOT EXISTS runtime_leases (
                lease_key      TEXT PRIMARY KEY,
                owner_id       TEXT NOT NULL,
                owner_instance TEXT NOT NULL DEFAULT '',
                kind           TEXT NOT NULL,
                session_id     TEXT,
                lease_token    TEXT NOT NULL DEFAULT '',
                fencing_token  INTEGER NOT NULL DEFAULT 0,
                acquired_at    TEXT NOT NULL,
                heartbeat_at   TEXT NOT NULL,
                renewed_at     TEXT NOT NULL DEFAULT '',
                expires_at     TEXT NOT NULL,
                released_at    TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_runtime_leases_expiry
                ON runtime_leases(expires_at);
            CREATE INDEX IF NOT EXISTS idx_runtime_leases_session
                ON runtime_leases(session_id);
            -- Fencing generations survive lease-row cleanup. A released or
            -- expired row may be deleted, but the next acquisition for the key
            -- must still advance monotonically.
            CREATE TABLE IF NOT EXISTS runtime_lease_fences (
                lease_key     TEXT PRIMARY KEY,
                fencing_token INTEGER NOT NULL DEFAULT 0,
                updated_at    TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runtime_lease_audit (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                lease_key     TEXT NOT NULL,
                kind          TEXT NOT NULL,
                action        TEXT NOT NULL,
                owner_id      TEXT NOT NULL,
                owner_instance TEXT NOT NULL DEFAULT '',
                fencing_token INTEGER NOT NULL DEFAULT 0,
                detail        TEXT,
                occurred_at   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_runtime_lease_audit_key
                ON runtime_lease_audit(lease_key, id DESC);
            CREATE INDEX IF NOT EXISTS idx_runtime_lease_audit_time
                ON runtime_lease_audit(occurred_at);

            CREATE TABLE IF NOT EXISTS computer_leases (
                lease_id      TEXT PRIMARY KEY,
                surface       TEXT NOT NULL UNIQUE,
                owner_id      TEXT NOT NULL,
                session_id    TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                agent_id      TEXT NOT NULL,
                agent_name    TEXT NOT NULL,
                acquired_at   TEXT NOT NULL,
                heartbeat_at  TEXT NOT NULL,
                expires_at    TEXT NOT NULL,
                generation    INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_computer_leases_expiry
                ON computer_leases(expires_at);
            CREATE INDEX IF NOT EXISTS idx_computer_leases_session
                ON computer_leases(session_id);
            "#,
        )?;
        let fts_migrated = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = 'memory_fts'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if fts_migrated.is_none() {
            conn.execute(
                "INSERT INTO memories_fts(memories_fts) VALUES('rebuild')",
                [],
            )?;
            conn.execute(
                "INSERT INTO store_meta (key, value) VALUES ('memory_fts', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        let search_fts_migrated = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key = 'search_fts'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if search_fts_migrated.is_none() {
            conn.execute(
                "INSERT INTO messages_fts(messages_fts) VALUES('rebuild')",
                [],
            )?;
            conn.execute("INSERT INTO events_fts(events_fts) VALUES('rebuild')", [])?;
            conn.execute(
                "INSERT INTO store_meta (key, value) VALUES ('search_fts', '1')
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [],
            )?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_approvals_status ON approvals(status);
             CREATE INDEX IF NOT EXISTS idx_questions_status ON questions(status);
             CREATE INDEX IF NOT EXISTS idx_merge_queue_status ON merge_queue(status);
             CREATE INDEX IF NOT EXISTS idx_sessions_status ON sessions(status);",
        )?;
        ensure_column(
            &conn,
            "project_task_scheduler_policies",
            "dispatch_tasks",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_task_scheduler_policies",
            "include_intake",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(&conn, "intake_tasks", "deps", "TEXT NOT NULL DEFAULT '[]'")?;
        ensure_column(
            &conn,
            "session_continuations",
            "series_id",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "parent_continuation_id",
            "TEXT",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "recurrence_every_secs",
            "INTEGER",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "recurrence_max_runs",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "run_count",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "session_continuations", "lease_owner", "TEXT")?;
        ensure_column(&conn, "session_continuations", "lease_token", "TEXT")?;
        ensure_column(&conn, "session_continuations", "lease_expires_at", "TEXT")?;
        ensure_column(&conn, "session_continuations", "watch_gate_id", "TEXT")?;
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_session_continuations_gate_watch
                 ON session_continuations(watch_gate_id)
                 WHERE watch_gate_id IS NOT NULL
                   AND status IN ('pending', 'ready', 'delivering');",
        )?;
        ensure_column(
            &conn,
            "session_continuations",
            "delivery_attempt",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "owner_instance",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "lease_token",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "fencing_token",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "runtime_leases",
            "renewed_at",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(&conn, "runtime_leases", "released_at", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_runtime_leases_instance
                 ON runtime_leases(owner_instance, expires_at);
             CREATE INDEX IF NOT EXISTS idx_session_continuations_claim
                 ON session_continuations(status, lease_expires_at);
             CREATE TABLE IF NOT EXISTS runtime_lease_fences (
                 lease_key     TEXT PRIMARY KEY,
                 fencing_token INTEGER NOT NULL DEFAULT 0,
                 updated_at    TEXT NOT NULL
             );",
        )?;
        ensure_column(
            &conn,
            "computer_leases",
            "generation",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "sources", "collection", "TEXT")?;
        ensure_column(
            &conn,
            "project_task_scheduler_policies",
            "max_intake_parallel",
            "INTEGER NOT NULL DEFAULT 2",
        )?;
        ensure_column(&conn, "sessions", "priority", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "sessions", "worktree", "TEXT")?;
        ensure_column(&conn, "sessions", "branch", "TEXT")?;
        ensure_column(&conn, "sessions", "token_budget", "INTEGER")?;
        ensure_column(&conn, "sessions", "cost_budget_usd", "REAL")?;
        ensure_column(&conn, "sessions", "time_budget_secs", "INTEGER")?;
        ensure_column(&conn, "sessions", "unlimited", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(
            &conn,
            "sessions",
            "cost_spent_usd",
            "REAL NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "sessions", "base_commit", "TEXT")?;
        ensure_column(&conn, "sessions", "team_plan", "TEXT")?;
        ensure_column(&conn, "sessions", "plan_runtime_snapshot", "TEXT")?;
        // Existing sessions default to the legacy layout. New rows are written
        // with the project default (normally 3) by `insert_session`.
        ensure_column(
            &conn,
            "sessions",
            "prompt_layout",
            "INTEGER NOT NULL DEFAULT 2",
        )?;
        ensure_column(
            &conn,
            "artifacts",
            "storage",
            "TEXT NOT NULL DEFAULT 'inline'",
        )?;
        ensure_column(
            &conn,
            "artifacts",
            "size_bytes",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "artifacts", "sha256", "TEXT NOT NULL DEFAULT ''")?;
        ensure_column(&conn, "artifacts", "path", "TEXT")?;
        ensure_column(&conn, "artifacts", "version", "INTEGER NOT NULL DEFAULT 1")?;
        ensure_column(&conn, "artifacts", "provenance", "TEXT NOT NULL DEFAULT ''")?;
        ensure_column(
            &conn,
            "artifacts",
            "compression",
            "TEXT NOT NULL DEFAULT 'none'",
        )?;
        ensure_column(
            &conn,
            "artifact_versions",
            "provenance",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(&conn, "usage_ledger", "department", "TEXT")?;
        ensure_column(
            &conn,
            "usage_ledger",
            "cache_read_tokens",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "usage_ledger",
            "cache_write_tokens",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_shell_policies",
            "restrict_home",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_shell_policies",
            "read_jail",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_mailbox_policies",
            "delete_unread",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_mailbox_policies",
            "auto_gc",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "agents", "name", "TEXT NOT NULL DEFAULT 'Agent'")?;
        ensure_column(&conn, "agents", "title", "TEXT")?;
        ensure_column(&conn, "agents", "profile", "TEXT")?;
        ensure_column(&conn, "agents", "parent_id", "TEXT")?;
        ensure_column(&conn, "agents", "worktree", "TEXT")?;
        ensure_column(&conn, "agents", "branch", "TEXT")?;
        ensure_column(&conn, "agents", "task_id", "TEXT")?;
        ensure_column(&conn, "agents", "department", "TEXT")?;
        // Nullable by design: every pre-S1 run record and every S1 run keeps
        // `NULL` because session assignment is a later phase. No synthetic
        // identities are backfilled.
        ensure_column(&conn, "agents", "identity_id", "TEXT")?;
        ensure_column(&conn, "agents", "workspace_id", "TEXT")?;
        ensure_column(&conn, "agents", "parent_workspace_id", "TEXT")?;
        ensure_column(
            &conn,
            "agent_identities",
            "origin",
            "TEXT NOT NULL DEFAULT 'local'",
        )?;
        conn.execute_batch(
            "CREATE VIEW IF NOT EXISTS agent_runs AS
                 SELECT id,
                        identity_id,
                        session_id,
                        task_id,
                        role,
                        status,
                        worktree,
                        branch,
                        created_at AS started_at,
                        NULL AS ended_at
                 FROM agents;",
        )?;
        ensure_column(
            &conn,
            "acp_agents",
            "transport",
            "TEXT NOT NULL DEFAULT 'stdio'",
        )?;
        ensure_column(&conn, "acp_agents", "endpoint", "TEXT")?;
        ensure_column(&conn, "acp_agents", "auth_header_env", "TEXT")?;
        ensure_column(&conn, "acp_agents", "headers", "TEXT NOT NULL DEFAULT '{}'")?;
        ensure_column(
            &conn,
            "acp_agents",
            "connect_timeout_secs",
            "INTEGER NOT NULL DEFAULT 30",
        )?;
        ensure_column(
            &conn,
            "acp_agents",
            "allow_remote",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "memories",
            "status",
            "TEXT NOT NULL DEFAULT 'active'",
        )?;
        ensure_column(&conn, "memories", "source", "TEXT NOT NULL DEFAULT 'agent'")?;
        ensure_column(&conn, "memories", "evidence", "TEXT NOT NULL DEFAULT '[]'")?;
        ensure_column(
            &conn,
            "memories",
            "supersedes",
            "TEXT NOT NULL DEFAULT '[]'",
        )?;
        ensure_column(&conn, "memories", "superseded_by", "TEXT")?;
        ensure_column(&conn, "memories", "pinned", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "memories", "use_count", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "memories", "last_used_at", "TEXT")?;
        ensure_column(&conn, "memories", "last_verified_at", "TEXT")?;
        ensure_column(&conn, "memories", "verify_after", "TEXT")?;
        ensure_column(&conn, "memories", "stale_reason", "TEXT")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_memories_project_status
                 ON memories(project_id, status, created_at);
             CREATE INDEX IF NOT EXISTS idx_memories_project_kind_title
                 ON memories(project_id, kind, title);
             CREATE TABLE IF NOT EXISTS memory_usage (
                 memory_id  TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                 session_id TEXT NOT NULL,
                 used_at    TEXT NOT NULL,
                 PRIMARY KEY(memory_id, session_id)
             );
             CREATE TABLE IF NOT EXISTS memory_conflicts (
                 id               TEXT PRIMARY KEY,
                 project_id       TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                 session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 memory_a_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                 memory_b_id      TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
                 memory_a_title   TEXT NOT NULL,
                 memory_b_title   TEXT NOT NULL,
                 memory_a_kind    TEXT NOT NULL,
                 memory_b_kind    TEXT NOT NULL,
                 memory_a_excerpt TEXT NOT NULL,
                 memory_b_excerpt TEXT NOT NULL,
                 scope_kind       TEXT NOT NULL,
                 scope_value      TEXT NOT NULL,
                 reason           TEXT NOT NULL,
                 status           TEXT NOT NULL DEFAULT 'open',
                 detected_at      TEXT NOT NULL,
                 resolved_at      TEXT,
                 resolution       TEXT,
                 note             TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_memory_conflicts_project_status
                 ON memory_conflicts(project_id, status, detected_at);
             CREATE TABLE IF NOT EXISTS memory_embeddings (
                 memory_id    TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 model        TEXT NOT NULL,
                 dimensions   INTEGER NOT NULL,
                 content_hash TEXT NOT NULL,
                 vector       BLOB NOT NULL,
                 created_at   TEXT NOT NULL,
                 updated_at   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embeddings_model
                 ON memory_embeddings(model, dimensions);
             CREATE TABLE IF NOT EXISTS memory_embedding_jobs (
                 memory_id       TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 model           TEXT NOT NULL,
                 status          TEXT NOT NULL,
                 attempts        INTEGER NOT NULL DEFAULT 0,
                 last_error      TEXT,
                 next_attempt_at TEXT,
                 updated_at      TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embedding_jobs_status
                 ON memory_embedding_jobs(status, next_attempt_at, updated_at);
             CREATE TABLE IF NOT EXISTS memory_embedding_usage (
                 id                 TEXT PRIMARY KEY,
                 project_id         TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                 model              TEXT NOT NULL,
                 calls              INTEGER NOT NULL,
                 input_tokens       INTEGER NOT NULL,
                 estimated_tokens   INTEGER NOT NULL,
                 bytes              INTEGER NOT NULL,
                 provider_reported  INTEGER NOT NULL,
                 created_at         TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embedding_usage_project
                 ON memory_embedding_usage(project_id, model, created_at);
             CREATE TABLE IF NOT EXISTS memory_embedding_index (
                 project_id           TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                 model                TEXT NOT NULL,
                 dimensions           INTEGER NOT NULL,
                 generation           INTEGER NOT NULL,
                 nlist                INTEGER NOT NULL,
                 cluster_count        INTEGER NOT NULL,
                 vector_count         INTEGER NOT NULL,
                 centroids            BLOB NOT NULL,
                 updated_at           TEXT NOT NULL,
                 last_error           TEXT,
                 fallback_count       INTEGER NOT NULL DEFAULT 0,
                 last_fallback_at     TEXT,
                 last_fallback_reason TEXT
             );
             CREATE TABLE IF NOT EXISTS memory_embedding_assignments (
                 memory_id  TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 generation INTEGER NOT NULL,
                 cluster    INTEGER NOT NULL,
                 updated_at TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_memory_embedding_assignments_generation
                 ON memory_embedding_assignments(generation, cluster);",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "task_result_verify_days",
            "INTEGER NOT NULL DEFAULT 90",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "default_verify_days",
            "INTEGER NOT NULL DEFAULT 365",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "auto_supersede",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "brief_max_chars",
            "INTEGER NOT NULL DEFAULT 4000",
        )?;
        ensure_column(
            &conn,
            "project_memory_policies",
            "sweep_stale",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "project_memory_policies", "embedding_json", "TEXT")?;
        ensure_column(&conn, "merge_queue", "pre_merge_commit", "TEXT")?;
        ensure_column(
            &conn,
            "merge_queue",
            "source",
            "TEXT NOT NULL DEFAULT 'local'",
        )?;
        ensure_column(&conn, "merge_queue", "remote_host", "TEXT")?;
        ensure_column(&conn, "merge_queue", "remote_branch", "TEXT")?;
        ensure_column(&conn, "merge_queue", "bundle_sha256", "TEXT")?;
        ensure_column(&conn, "merge_queue", "imported_ref", "TEXT")?;
        ensure_column(&conn, "merge_queue", "verifier_approved", "INTEGER")?;
        ensure_column(&conn, "merge_queue", "verifier_summary", "TEXT")?;
        ensure_column(&conn, "merge_queue", "environment_caveat", "TEXT")?;
        ensure_column(&conn, "merge_queue", "remote_test_evidence_id", "TEXT")?;
        ensure_column(&conn, "merge_queue", "remote_test_digest", "TEXT")?;
        ensure_column(&conn, "merge_queue", "remote_test_status", "TEXT")?;

        ensure_column(
            &conn,
            "project_process_policies",
            "restart_on_boot",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "project_process_policies",
            "restart_max_attempts",
            "INTEGER NOT NULL DEFAULT 3",
        )?;
        ensure_column(
            &conn,
            "project_process_policies",
            "restart_backoff_secs",
            "INTEGER NOT NULL DEFAULT 5",
        )?;
        ensure_column(
            &conn,
            "supervised_processes",
            "restart_attempts",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "supervised_processes",
            "restart_blocked_reason",
            "TEXT",
        )?;
        ensure_column(&conn, "plan_tasks", "next_attempt_at", "TEXT")?;
        ensure_column(
            &conn,
            "effect_receipts",
            "settled_by_sweep",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "scaling_decisions", "replay_inputs_json", "TEXT")?;
        ensure_column(&conn, "session_runs", "runtime_snapshot_json", "TEXT")?;
        ensure_column(&conn, "approvals", "approval_risk", "TEXT")?;
        ensure_column(&conn, "approvals", "approval_reason", "TEXT")?;
        ensure_column(&conn, "approvals", "agent_id", "TEXT")?;
        ensure_column(&conn, "approvals", "agent_name", "TEXT")?;
        ensure_column(&conn, "approvals", "agent_role", "TEXT")?;
        ensure_column(&conn, "approvals", "agent_title", "TEXT")?;
        ensure_column(&conn, "approvals", "task_id", "TEXT")?;
        ensure_column(&conn, "approvals", "scope_hash", "TEXT")?;
        ensure_column(&conn, "approvals", "dedup_key", "TEXT")?;
        ensure_column(
            &conn,
            "approvals",
            "waiter_count",
            "INTEGER NOT NULL DEFAULT 1",
        )?;
        ensure_column(&conn, "approvals", "template_id", "TEXT")?;
        ensure_column(&conn, "approvals", "priority", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "approvals", "expires_at", "TEXT")?;
        ensure_column(&conn, "approvals", "notify", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "approvals", "notify_key", "TEXT")?;
        ensure_column(&conn, "sessions", "tokens_in", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(
            &conn,
            "sessions",
            "tokens_out",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "mcp_servers",
            "transport",
            "TEXT NOT NULL DEFAULT 'stdio'",
        )?;
        ensure_column(&conn, "mcp_servers", "url", "TEXT")?;
        ensure_column(
            &conn,
            "project_team_templates",
            "origin",
            "TEXT NOT NULL DEFAULT 'local'",
        )?;
        ensure_column(
            &conn,
            "mcp_servers",
            "headers",
            "TEXT NOT NULL DEFAULT '{}'",
        )?;
        let daemon_identity = crate::store::lease::load_or_create_daemon_identity(&conn)?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            data_dir,
            mirror_lock: Arc::new(Mutex::new(())),
            manifest_lock: Arc::new(Mutex::new(())),
            daemon_identity,
        })
    }
}

use super::*;

impl Engine {
    /// Run one bounded browser action for an agent. The helper is session- and
    /// worktree-scoped and uses a non-persistent WebKit store. The physical GUI
    /// is owned by the unified `gui` lease for the action plus its project
    /// grace window; the warm helper itself holds no lease.
    pub async fn browser_action(
        &self,
        session_id: &str,
        worktree: &std::path::Path,
        agent_id: &str,
        agent_name: &str,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let target = browser::BrowserTarget {
            session_id,
            worktree,
            agent_id,
            agent_name,
        };
        self.browsers.action(self, &target, input).await
    }

    /// Stop browser helpers owned by a session before scratch cleanup and
    /// release the GUI lease immediately.
    pub async fn stop_session_browsers(&self, session_id: &str) -> usize {
        self.gui_end_session(session_id, "session_cleanup").await;
        self.browsers.stop_session(session_id).await
    }

    /// Stop browsers idle beyond the configured TTL.
    pub async fn sweep_idle_browsers(&self, idle: std::time::Duration) -> usize {
        self.browsers.sweep_idle(idle).await
    }

    /// Run one bounded native computer action for an agent. The helper is
    /// session- and worktree-scoped and runs with the logged-in user's
    /// authority. The unified `gui` lease serializes it with the browser.
    pub async fn computer_action(
        &self,
        session_id: &str,
        worktree: &std::path::Path,
        agent_id: &str,
        agent_name: &str,
        task_id: Option<&str>,
        input: serde_json::Value,
    ) -> Result<ToolOutput> {
        let target = computer::ComputerTarget {
            session_id,
            worktree,
            agent_id,
            agent_name,
            task_id,
        };
        self.computers.action(self, &target, input).await
    }

    /// Inspect helper availability and TCC permissions without taking the GUI
    /// lease or posting any input.
    pub async fn computer_status(&self) -> vp_protocol::types::ComputerStatusResult {
        self.computer_status_for(None, false).await
    }

    /// Cached helper/TCC status. When `session_id` is present its project
    /// policy controls the cache TTL and grace hint.
    pub async fn computer_status_for(
        &self,
        session_id: Option<&str>,
        refresh: bool,
    ) -> vp_protocol::types::ComputerStatusResult {
        let policy = match session_id {
            Some(session_id) => self
                .gui_policy_for_session(session_id)
                .await
                .unwrap_or_default(),
            None => vp_protocol::types::GuiPolicy::default(),
        };
        self.computers.status(&policy, refresh).await
    }

    /// Stop computer helpers owned by a session before scratch cleanup and
    /// release the GUI lease immediately.
    pub async fn stop_session_computers(&self, session_id: &str) -> usize {
        self.gui_end_session(session_id, "session_cleanup").await;
        self.computers.stop_session(session_id).await
    }

    /// Stop computer helpers idle beyond the configured TTL.
    pub async fn sweep_idle_computers(&self, idle: std::time::Duration) -> usize {
        self.computers.sweep_idle(idle).await
    }

    pub async fn console_open(&self, session_id: &str) -> Result<ConsoleOpenResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (session, project) = tokio::task::spawn_blocking(move || {
            let session = store
                .get_session(&id)?
                .ok_or_else(|| anyhow::anyhow!("session not found: {id}"))?;
            let project = store
                .get_project(&session.project_id)?
                .ok_or_else(|| anyhow::anyhow!("project not found"))?;
            Ok::<_, anyhow::Error>((session, project))
        })
        .await??;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let session = self.ensure_session_worktree(&session.id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        let policy = self.get_shell_policy(&project.id).await?;
        let mut secret_roots = crate::agent::shell_secret_roots(self.store.data_dir());
        secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
        let context = ToolContext {
            root: PathBuf::from(&worktree),
            project_root: PathBuf::from(&project.root),
            sandbox_root: self
                .store
                .data_dir()
                .join("sandbox")
                .join(&session.id)
                .join("console"),
            secret_roots,
            network_allowed: policy.network != "deny",
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: policy.restrict_home,
            read_jail: policy.read_jail,
            git_branch: session.branch.clone(),
            remote: None,
            session_id: None,
            agent_id: None,
            task_id: None,
            process_supervisor: None,
            test_cache: None,
        };
        self.consoles.open(&session.id, context).await
    }

    pub async fn console_write(&self, console_id: &str, data: &str) -> Result<ConsoleWriteResult> {
        let written = self.consoles.write(console_id, data).await?;
        Ok(ConsoleWriteResult { written })
    }

    pub async fn console_read(
        &self,
        console_id: &str,
        after_seq: u64,
    ) -> Result<ConsoleReadResult> {
        self.consoles.read(console_id, after_seq).await
    }

    pub async fn console_signal(
        &self,
        console_id: &str,
        signal: &str,
    ) -> Result<ConsoleSignalResult> {
        self.consoles.signal(console_id, signal).await
    }

    pub async fn console_close(&self, console_id: &str) -> Result<ConsoleCloseResult> {
        self.consoles.close(console_id).await
    }

    pub fn process_supervisor(&self) -> &Arc<process_supervisor::ProcessSupervisor> {
        &self.process_supervisor
    }

    /// Reconcile durable supervised processes after a daemon restart. Adoption
    /// requires full PID/start-time/nonce/command-hash evidence.
    pub async fn reconcile_supervised_processes(
        &self,
    ) -> Result<process_supervisor::ReconcileReport> {
        self.process_supervisor.reconcile().await
    }

    /// Idle-stop and retention-sweep durable processes. The daemon controls the
    /// cadence and can disable idle stops with
    /// `VERLINPARK_PROCESS_IDLE_SWEEP_SECS=0`.
    pub async fn sweep_supervised_processes(
        &self,
        idle_enabled: bool,
    ) -> Result<process_supervisor::SweepReport> {
        self.process_supervisor.sweep(idle_enabled).await
    }

    pub async fn stop_session_processes(&self, session_id: &str, reason: &str) -> usize {
        self.process_supervisor
            .stop_session(session_id, reason)
            .await
    }

    pub fn process_supervisor_status(&self) -> Result<ProcessSupervisorStatus> {
        self.process_supervisor.status()
    }

    pub async fn session_processes(
        &self,
        session_id: &str,
        include_finished: Option<bool>,
        limit: Option<u32>,
    ) -> Result<ProcessListResult> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || {
            if store.get_session(&id)?.is_none() {
                return Err(anyhow::anyhow!("session not found: {id}"));
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        let rows = self.process_supervisor.list(
            session_id,
            include_finished.unwrap_or(true),
            limit.unwrap_or(50).clamp(1, 200),
        )?;
        Ok(ProcessListResult {
            session_id: session_id.to_string(),
            active: self.process_supervisor.active_count(session_id)?,
            processes: rows,
            limit: self
                .process_supervisor
                .policy_for_session(session_id)
                .max_processes,
        })
    }

    pub async fn session_process_logs(
        &self,
        session_id: &str,
        process_id: &str,
        offset: Option<u64>,
        max_bytes: Option<u32>,
        max_lines: Option<u32>,
        from_end: bool,
    ) -> Result<ProcessLogsResult> {
        let supervisor = Arc::clone(&self.process_supervisor);
        let session_id = session_id.to_string();
        let process_id = process_id.to_string();
        tokio::task::spawn_blocking(move || {
            supervisor.read_logs(
                &session_id,
                &process_id,
                offset,
                max_bytes,
                max_lines,
                from_end,
            )
        })
        .await?
    }

    pub async fn session_process_stop(
        &self,
        session_id: &str,
        process_id: &str,
        signal: Option<String>,
        grace_secs: Option<u32>,
    ) -> Result<ProcessStopResult> {
        self.process_supervisor
            .stop(
                session_id,
                process_id,
                signal.as_deref(),
                grace_secs.map(u64::from),
                "stopped by operator",
            )
            .await
    }

    /// Explicit operator retry for a non-live supervised process. The
    /// supervisor still revalidates identity and reuses the boot-restart env,
    /// cwd, sandbox, and bounded attempt machinery.
    pub async fn session_process_restart(
        &self,
        session_id: &str,
        process_id: &str,
        confirm: bool,
    ) -> Result<ProcessRestartResult> {
        let supervisor = Arc::clone(&self.process_supervisor);
        let session = session_id.to_string();
        let process = process_id.to_string();
        tokio::task::spawn_blocking(move || supervisor.request_restart(&session, &process, confirm))
            .await?
    }

    pub async fn workspace_tree(&self, session_id: &str) -> Result<WorkspaceTreeResult> {
        let root = self.workspace_root(session_id).await?;
        tokio::task::spawn_blocking(move || workspace::tree(Path::new(&root))).await?
    }

    pub async fn workspace_list(
        &self,
        session_id: &str,
        path: &str,
        offset: Option<u32>,
        limit: Option<u32>,
    ) -> Result<WorkspaceListResult> {
        let root = self.workspace_root(session_id).await?;
        let path = path.to_string();
        tokio::task::spawn_blocking(move || workspace::list(Path::new(&root), &path, offset, limit))
            .await?
    }

    pub async fn workspace_search(
        &self,
        session_id: &str,
        query: &str,
        limit: Option<u32>,
    ) -> Result<WorkspaceSearchResult> {
        let root = self.workspace_root(session_id).await?;
        let query = query.to_string();
        tokio::task::spawn_blocking(move || workspace::search(Path::new(&root), &query, limit))
            .await?
    }

    pub async fn workspace_grep(
        &self,
        session_id: &str,
        query: &str,
        path: Option<String>,
        case_sensitive: Option<bool>,
        max_results: Option<u32>,
    ) -> Result<WorkspaceGrepResult> {
        let root = self.workspace_root(session_id).await?;
        let query = query.to_string();
        tokio::task::spawn_blocking(move || {
            workspace::grep(
                Path::new(&root),
                &query,
                path.as_deref(),
                case_sensitive.unwrap_or(false),
                max_results,
            )
        })
        .await?
    }

    pub async fn workspace_file(
        &self,
        session_id: &str,
        path: &str,
    ) -> Result<WorkspaceFileResult> {
        let root = self.workspace_root(session_id).await?;
        let path = path.to_string();
        tokio::task::spawn_blocking(move || workspace::read(Path::new(&root), &path)).await?
    }

    pub async fn workspace_write(
        &self,
        session_id: &str,
        path: &str,
        content: String,
        expected_sha256: Option<String>,
    ) -> Result<WorkspaceWriteResult> {
        let root = self.workspace_root(session_id).await?;
        let path_for_write = path.to_string();
        let expected = expected_sha256.clone();
        let result = tokio::task::spawn_blocking(move || {
            workspace::write(
                Path::new(&root),
                &path_for_write,
                &content,
                expected.as_deref(),
            )
        })
        .await??;
        if let Err(error) = self
            .append_event(
                session_id,
                "workspace.file_saved",
                serde_json::json!({
                    "path": result.path,
                    "size": result.size,
                    "sha256": result.sha256,
                }),
            )
            .await
        {
            tracing::warn!(%error, "failed to append workspace save event");
        }
        Ok(result)
    }

    async fn workspace_root(&self, session_id: &str) -> Result<String> {
        Ok(self.workspace_session(session_id).await?.1)
    }

    async fn workspace_session(&self, session_id: &str) -> Result<(Session, String)> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let session = tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let session = self.ensure_session_worktree(&session.id).await?;
        let worktree = session
            .worktree
            .clone()
            .ok_or_else(|| anyhow::anyhow!("session has no worktree"))?;
        Ok((session, worktree))
    }

    pub async fn git_log(&self, session_id: &str, limit: u32) -> Result<GitLogResult> {
        let (session, root) = self.workspace_session(session_id).await?;
        let commits =
            tokio::task::spawn_blocking(move || history::log(Path::new(&root), limit)).await??;
        Ok(GitLogResult {
            session_id: session_id.to_string(),
            branch: session.branch,
            base_commit: session.base_commit,
            commits,
        })
    }

    pub async fn git_show(&self, session_id: &str, commit: &str) -> Result<GitShowResult> {
        let (_, root) = self.workspace_session(session_id).await?;
        let commit = commit.to_string();
        let mut result =
            tokio::task::spawn_blocking(move || history::show(Path::new(&root), &commit)).await??;
        result.session_id = session_id.to_string();
        Ok(result)
    }

    pub async fn terminal_open(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalOpenResult> {
        let (session, worktree) = self.workspace_session(session_id).await?;
        let store = self.store.clone();
        let project_id = session.project_id.clone();
        let project = tokio::task::spawn_blocking(move || store.get_project(&project_id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("project not found"))?;
        let policy = self.get_shell_policy(&project.id).await?;
        let mut secret_roots = crate::agent::shell_secret_roots(self.store.data_dir());
        secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
        let context = ToolContext {
            root: PathBuf::from(&worktree),
            project_root: PathBuf::from(&project.root),
            sandbox_root: self
                .store
                .data_dir()
                .join("sandbox")
                .join(&session.id)
                .join("terminal"),
            secret_roots,
            network_allowed: policy.network != "deny",
            home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default(),
            restrict_home: policy.restrict_home,
            read_jail: policy.read_jail,
            git_branch: session.branch.clone(),
            remote: None,
            session_id: None,
            agent_id: None,
            task_id: None,
            process_supervisor: None,
            test_cache: None,
        };
        self.terminals.open(&session.id, context, cols, rows).await
    }

    pub async fn terminal_write(
        &self,
        terminal_id: &str,
        data: &str,
    ) -> Result<TerminalWriteResult> {
        self.terminals.write(terminal_id, data).await
    }

    pub async fn terminal_read(
        &self,
        terminal_id: &str,
        after_seq: u64,
    ) -> Result<TerminalReadResult> {
        self.terminals.read(terminal_id, after_seq).await
    }

    pub async fn terminal_resize(
        &self,
        terminal_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalResizeResult> {
        self.terminals.resize(terminal_id, cols, rows).await
    }

    pub async fn terminal_close(&self, terminal_id: &str) -> Result<TerminalCloseResult> {
        self.terminals.close(terminal_id).await
    }

    /// Sweep consoles and terminals that have been idle for `idle`. Called by
    /// the daemon so abandoned GUI sessions cannot accumulate child processes.
    pub async fn sweep_idle_terminals(&self, idle: std::time::Duration) -> (usize, usize) {
        let consoles = self.consoles.sweep_idle(idle).await;
        let terminals = self.terminals.sweep_idle(idle).await;
        (consoles, terminals)
    }
}

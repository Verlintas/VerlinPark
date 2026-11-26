use super::*;

impl Engine {
    pub async fn recover_interrupted_sessions(&self) -> Result<usize> {
        let store = self.store.clone();
        let (sessions, events) =
            tokio::task::spawn_blocking(move || store.recover_state()).await??;
        for event in events {
            let _ = self.events.send(event);
        }
        // Durable run journal recovery runs after session/agent status changes:
        // every run left `running` by the previous process becomes
        // `interrupted`, and its tool intents become unknown.
        let interrupted_runs = crate::run_journal::interrupt_running_runs(self).await?;
        if interrupted_runs > 0 {
            tracing::warn!(
                runs = interrupted_runs,
                "marked durable runs interrupted after daemon recovery"
            );
        }
        // An operation receipt that is still `pending` belongs to the previous
        // daemon process. Its side effect may or may not have landed, so it is
        // failed closed as `unknown` instead of being replayed.
        let operation_store = self.store.clone();
        let unknown_operations = tokio::task::spawn_blocking(move || {
            operation_store.mark_pending_session_operations_unknown()
        })
        .await??;
        if unknown_operations > 0 {
            tracing::warn!(
                operations = unknown_operations,
                "marked pending session operations unknown after daemon recovery"
            );
        }
        // A crash may land after `git merge` but before the queue status is
        // finalized. If the session worktree is clean, reset it to the
        // recorded pre-merge HEAD so retry starts from a known state.
        let recovery_store = self.store.clone();
        let recoveries =
            tokio::task::spawn_blocking(move || recovery_store.interrupted_merge_recoveries())
                .await??;
        let git_lock = self.git_lock.clone();
        for entry in recoveries {
            let Some(pre_merge) = entry.pre_merge_commit.clone() else {
                continue;
            };
            let session_store = self.store.clone();
            let session_id = entry.session_id.clone();
            let session =
                tokio::task::spawn_blocking(move || session_store.get_session(&session_id))
                    .await??;
            let Some(session) = session else {
                continue;
            };
            let Some(worktree_path) = session.worktree.clone() else {
                continue;
            };
            let path = PathBuf::from(worktree_path);
            let reset = {
                let git_lock = git_lock.clone();
                let path = path.clone();
                let pre_merge = pre_merge.clone();
                tokio::task::spawn_blocking(move || -> Result<()> {
                    let _guard = git_lock
                        .lock()
                        .map_err(|_| anyhow::anyhow!("git mutex poisoned"))?;
                    if !worktree::is_clean(&path)? {
                        anyhow::bail!("session worktree is not clean");
                    }
                    worktree::reset_hard(&path, &pre_merge)
                })
                .await?
            };
            let (kind, payload) = match reset {
                Ok(()) => (
                    "merge_queue.recovered",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "pre_merge_commit": pre_merge,
                        "detail": "reset clean session worktree to the pre-merge HEAD",
                    }),
                ),
                Err(error) => (
                    "merge_queue.recovery_skipped",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "pre_merge_commit": pre_merge,
                        "detail": format!("{error:#}"),
                    }),
                ),
            };
            if let Err(error) = self.append_event(&session.id, kind, payload).await {
                tracing::warn!(%error, entry_id = %entry.id, "failed to record merge recovery");
            }
        }
        Ok(sessions.len())
    }

    pub async fn doctor_checks(&self) -> Result<Vec<DoctorCheck>> {
        let store = self.store.clone();
        let data_dir = store.data_dir().to_path_buf();
        let (integrity, projects, sessions, shell_policies) =
            tokio::task::spawn_blocking(move || {
                let projects = store.list_projects()?;
                let shell_policies = projects
                    .iter()
                    .map(|project| {
                        store
                            .get_shell_policy(&project.id)
                            .map(|policy| (project.name.clone(), policy))
                    })
                    .collect::<Result<Vec<_>>>()?;
                let sessions = store.list_sessions(None)?;
                Ok::<_, anyhow::Error>((
                    store.integrity_check()?,
                    projects,
                    sessions,
                    shell_policies,
                ))
            })
            .await??;
        let mut checks = Vec::new();
        checks.push(DoctorCheck {
            name: "database".into(),
            status: if integrity == "ok" { "ok" } else { "error" }.into(),
            detail: integrity,
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&data_dir)
                .map(|metadata| metadata.permissions().mode() & 0o777)
                .unwrap_or(0);
            checks.push(DoctorCheck {
                name: "data directory".into(),
                status: if mode & 0o077 == 0 { "ok" } else { "warn" }.into(),
                detail: format!("{} (mode {mode:o})", data_dir.display()),
            });
        }
        let missing_projects = projects
            .iter()
            .filter(|project| !Path::new(&project.root).is_dir())
            .count();
        checks.push(DoctorCheck {
            name: "project roots".into(),
            status: if missing_projects == 0 { "ok" } else { "warn" }.into(),
            detail: format!(
                "{} project(s), {missing_projects} missing root(s)",
                projects.len()
            ),
        });
        let git_failures = projects
            .iter()
            .filter_map(|project| {
                super::super::projects::preflight_project_root(Path::new(&project.root))
                    .err()
                    .map(|error| format!("{}: {error:#}", project.name))
            })
            .collect::<Vec<_>>();
        checks.push(DoctorCheck {
            name: "git projects".into(),
            status: if git_failures.is_empty() {
                "ok"
            } else {
                "warn"
            }
            .into(),
            detail: if git_failures.is_empty() {
                "all project roots are non-empty Git repositories with user.name/user.email"
                    .to_string()
            } else {
                format!(
                    "{} project(s) need Git repair: {}",
                    git_failures.len(),
                    git_failures
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" | ")
                )
            },
        });
        let permissive_shell = shell_policies
            .iter()
            .filter(|(_, policy)| {
                policy.network != "deny" && !policy.restrict_home && !policy.read_jail
            })
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        checks.push(DoctorCheck {
            name: "shell trust".into(),
            status: if permissive_shell.is_empty() {
                "ok"
            } else {
                "warn"
            }
            .into(),
            detail: if permissive_shell.is_empty() {
                "all projects use an explicit shell trust posture".to_string()
            } else {
                format!(
                    "{} project(s) use permissive shell defaults (network allow, no read jail): {}. Choose hardened setup or set a project shell policy.",
                    permissive_shell.len(),
                    permissive_shell.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
                )
            },
        });
        let missing_worktrees = sessions
            .iter()
            .filter(|session| session.status != "imported")
            .filter(|session| {
                session
                    .worktree
                    .as_deref()
                    .is_some_and(|path| !Path::new(path).exists())
            })
            .count();
        let active = sessions
            .iter()
            .filter(|session| matches!(session.status.as_str(), "running" | "queued" | "paused"))
            .count();
        checks.push(DoctorCheck {
            name: "sessions".into(),
            status: if active > 0 { "warn" } else { "ok" }.into(),
            detail: format!(
                "{} session(s), {active} active/queued/paused, {missing_worktrees} missing worktree(s)",
                sessions.len()
            ),
        });
        let mut mirror_mismatches = Vec::new();
        for session in &sessions {
            match self.session_log_status(&session.id).await {
                Ok(status) if !status.matches => mirror_mismatches.push(status.session_id),
                Ok(_) => {}
                Err(error) => mirror_mismatches.push(format!("{}: {error}", session.id)),
            }
        }
        checks.push(DoctorCheck {
            name: "event mirrors".into(),
            status: if mirror_mismatches.is_empty() {
                "ok"
            } else {
                "warn"
            }
            .into(),
            detail: if mirror_mismatches.is_empty() {
                "all mirrors match".to_string()
            } else {
                format!(
                    "{} mirror mismatch(es): {}",
                    mirror_mismatches.len(),
                    mirror_mismatches
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            },
        });
        let sandbox = Path::new("/usr/bin/sandbox-exec").is_file();
        checks.push(DoctorCheck {
            name: "shell sandbox".into(),
            status: if sandbox { "ok" } else { "warn" }.into(),
            detail: if sandbox {
                "/usr/bin/sandbox-exec is available (deprecated by macOS)".to_string()
            } else {
                "sandbox-exec is missing; shell execution fails closed".to_string()
            },
        });
        Ok(checks)
    }

    pub async fn session_log_status(&self, session_id: &str) -> Result<SessionLogStatus> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let (mirror, database) = tokio::task::spawn_blocking(move || {
            let mirror = store.read_event_mirror(&id)?;
            let database = store.list_all_events(&id)?;
            Ok::<_, anyhow::Error>((mirror, database))
        })
        .await??;
        let mirror_count = mirror.as_ref().map_or(0, Vec::len);
        let matches = match &mirror {
            None => database.is_empty(),
            Some(mirror) => {
                mirror.len() == database.len()
                    && mirror.iter().zip(database.iter()).all(|(left, right)| {
                        left.seq == right.seq
                            && left.kind == right.kind
                            && left.payload == right.payload
                            && left.created_at == right.created_at
                    })
            }
        };
        let detail = if matches {
            "event mirror matches the database".to_string()
        } else if mirror.is_none() {
            format!(
                "event mirror is missing; database has {} event(s)",
                database.len()
            )
        } else if mirror_count != database.len() {
            format!(
                "event mirror has {} event(s); database has {}",
                mirror_count,
                database.len()
            )
        } else {
            "event mirror differs from the database".to_string()
        };
        Ok(SessionLogStatus {
            session_id: session_id.to_string(),
            mirror_events: mirror_count,
            database_events: database.len(),
            matches,
            detail,
        })
    }

    pub async fn restore_session_log(&self, session_id: &str) -> Result<SessionLogStatus> {
        ensure_session_not_active(&self.store, session_id, "restore an event log").await?;
        let store = self.store.clone();
        let id = session_id.to_string();
        let mirror = tokio::task::spawn_blocking(move || store.read_event_mirror(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("event mirror is missing for {session_id}"))?;
        if mirror.is_empty() {
            anyhow::bail!("event mirror is empty for {session_id}");
        }
        let mut expected_seq = 1u64;
        for event in &mirror {
            if event.session_id != session_id {
                anyhow::bail!("event mirror contains an event for a different session");
            }
            if event.seq != expected_seq {
                anyhow::bail!(
                    "event mirror sequence is not contiguous at {} (expected {}, found {})",
                    event.seq,
                    expected_seq,
                    event.seq
                );
            }
            expected_seq = expected_seq.saturating_add(1);
        }
        let store = self.store.clone();
        let events = mirror.clone();
        let log_session_id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.replace_events(&log_session_id, &events))
            .await??;
        self.rebuild_session(session_id).await?;
        self.session_log_status(session_id).await
    }
}

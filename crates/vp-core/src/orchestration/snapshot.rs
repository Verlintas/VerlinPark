use std::collections::{HashMap, HashSet};

use vp_protocol::now_rfc3339;
use vp_protocol::types::{
    AgentRecord, Event, MergeQueueEntry, OrchestrationBlocker, OrchestrationDecision,
    OrchestrationDepartment, OrchestrationMetrics, OrchestrationNote, OrchestrationPolicy,
    OrchestrationSnapshot, OrchestrationTask, Question, ScheduledTask, Session, TeamPlan,
};

use super::plan::{
    blocked_by, latest_result_summary, plan_revision, revisions_from_events, status_map,
};

pub struct OrchestrationInput<'a> {
    pub session: &'a Session,
    pub plan: Option<&'a TeamPlan>,
    pub scheduled: &'a [ScheduledTask],
    pub agents: &'a [AgentRecord],
    pub events: &'a [Event],
    pub merge_queue: &'a [MergeQueueEntry],
    pub questions: &'a [Question],
    pub notes: &'a [OrchestrationNote],
    pub decisions: &'a [OrchestrationDecision],
    pub policy: &'a OrchestrationPolicy,
}

pub fn build_snapshot(input: OrchestrationInput<'_>) -> OrchestrationSnapshot {
    let plan = input.plan;
    let revisions = revisions_from_events(input.events);
    let revision = plan_revision(input.events);
    let statuses = plan
        .map(|plan| status_map(plan, input.scheduled, input.events))
        .unwrap_or_default();
    let scheduled_by_task = input
        .scheduled
        .iter()
        .map(|task| (task.task_id.as_str(), task))
        .collect::<HashMap<_, _>>();
    let agent_by_task = owner_agents(input.agents);
    let verifier_status = verifier_status_by_task(input.agents, input.events);
    let notes = input.notes;
    let open_notes = notes
        .iter()
        .filter(|note| note.status == "open")
        .collect::<Vec<_>>();

    let tasks = plan
        .map(|plan| {
            plan.tasks
                .iter()
                .map(|task| {
                    let status = statuses
                        .get(&task.id)
                        .cloned()
                        .unwrap_or_else(|| "pending".to_string());
                    let scheduled = scheduled_by_task.get(task.id.as_str()).copied();
                    let owner = agent_by_task.get(task.id.as_str());
                    let blocked = blocked_by(task, &statuses);
                    let ready = matches!(
                        status.as_str(),
                        "pending" | "interrupted" | "failed" | "cancelled" | "retry_scheduled"
                    ) && blocked.is_empty();
                    OrchestrationTask {
                        task_id: task.id.clone(),
                        department: task.department.clone(),
                        title: task.title.clone(),
                        kind: task.kind.clone(),
                        priority: task.priority,
                        status,
                        deps: task.deps.clone(),
                        write_scope: task.write_scope.clone(),
                        acceptance: task.acceptance.clone(),
                        attempts: scheduled.map(|task| task.attempts).unwrap_or(0),
                        max_attempts: input.policy.max_attempts,
                        owner_agent_id: owner.map(|agent| agent.id.clone()),
                        owner_agent_name: owner.map(|agent| agent.name.clone()),
                        last_error: scheduled.and_then(|task| task.last_error.clone()),
                        result_summary: latest_result_summary(input.events, &task.id),
                        verifier_status: verifier_status.get(task.id.as_str()).cloned(),
                        next_attempt_at: scheduled.and_then(|task| task.next_attempt_at.clone()),
                        ready,
                        blocked_by: blocked,
                        started_at: scheduled.and_then(|task| task.started_at.clone()),
                        finished_at: scheduled.and_then(|task| task.finished_at.clone()),
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let departments = plan
        .map(|plan| {
            plan.departments
                .iter()
                .map(|department| {
                    let department_tasks = tasks
                        .iter()
                        .filter(|task| task.department == department.name)
                        .collect::<Vec<_>>();
                    let manager = input.agents.iter().find(|agent| {
                        agent.role == "manager"
                            && agent
                                .department
                                .as_deref()
                                .is_some_and(|name| name.eq_ignore_ascii_case(&department.name))
                    });
                    OrchestrationDepartment {
                        name: department.name.clone(),
                        manager_title: department.manager_title.clone(),
                        profile: department.profile.clone(),
                        objective: department.objective.clone(),
                        total_tasks: department_tasks.len(),
                        completed_tasks: department_tasks
                            .iter()
                            .filter(|task| matches!(task.status.as_str(), "completed" | "skipped"))
                            .count(),
                        active_tasks: department_tasks
                            .iter()
                            .filter(|task| {
                                matches!(
                                    task.status.as_str(),
                                    "running" | "queued" | "paused" | "pausing" | "cancelling"
                                )
                            })
                            .count(),
                        blocked_tasks: department_tasks
                            .iter()
                            .filter(|task| task.status == "blocked")
                            .count(),
                        failed_tasks: department_tasks
                            .iter()
                            .filter(|task| task.status == "failed")
                            .count(),
                        manager_agent_id: manager.map(|agent| agent.id.clone()),
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut blockers = Vec::new();
    for task in &tasks {
        if matches!(task.status.as_str(), "failed" | "blocked") {
            blockers.push(OrchestrationBlocker {
                kind: if task.status == "failed" {
                    "task_failed".to_string()
                } else {
                    "task_blocked".to_string()
                },
                note_id: None,
                task_id: Some(task.task_id.clone()),
                agent_id: task.owner_agent_id.clone(),
                agent_name: task.owner_agent_name.clone(),
                message: task
                    .last_error
                    .clone()
                    .unwrap_or_else(|| format!("{} is {}", task.task_id, task.status)),
                created_at: task
                    .finished_at
                    .clone()
                    .unwrap_or_else(|| input.session.created_at.clone()),
            });
        }
    }
    for entry in input.merge_queue {
        if matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted") {
            blockers.push(OrchestrationBlocker {
                kind: "merge_queue".to_string(),
                note_id: None,
                task_id: entry.task_id.clone(),
                agent_id: Some(entry.agent_id.clone()),
                agent_name: Some(entry.agent_name.clone()),
                message: entry
                    .detail
                    .clone()
                    .unwrap_or_else(|| format!("merge queue entry is {}", entry.status)),
                created_at: entry.updated_at.clone(),
            });
        }
    }
    for question in input
        .questions
        .iter()
        .filter(|question| question.status == "pending")
    {
        blockers.push(OrchestrationBlocker {
            kind: "user_question".to_string(),
            note_id: Some(question.id.clone()),
            task_id: None,
            agent_id: Some(question.agent_id.clone()),
            agent_name: Some(question.agent_name.clone()),
            message: question.question.clone(),
            created_at: question.created_at.clone(),
        });
    }
    for note in &open_notes {
        blockers.push(OrchestrationBlocker {
            kind: "coordination".to_string(),
            note_id: Some(note.note_id.clone()),
            task_id: note.task_id.clone(),
            agent_id: Some(note.agent_id.clone()),
            agent_name: Some(note.agent_name.clone()),
            message: note.message.clone(),
            created_at: note.created_at.clone(),
        });
    }
    for agent in input.agents {
        if matches!(
            agent.status.as_str(),
            "failed" | "cancelled" | "interrupted" | "blocked"
        ) {
            blockers.push(OrchestrationBlocker {
                kind: "agent_failed".to_string(),
                note_id: None,
                task_id: agent.task_id.clone(),
                agent_id: Some(agent.id.clone()),
                agent_name: Some(agent.name.clone()),
                message: format!("{} is {}", agent.name, agent.status),
                created_at: agent.created_at.clone(),
            });
        }
    }

    let failed_agents = input
        .agents
        .iter()
        .filter(|agent| {
            matches!(
                agent.status.as_str(),
                "failed" | "cancelled" | "interrupted" | "blocked"
            )
        })
        .count();
    let merge_conflicts = input
        .merge_queue
        .iter()
        .filter(|entry| entry.status == "conflict")
        .count();
    let ready = tasks.iter().filter(|task| task.ready).count();
    let completed = tasks
        .iter()
        .filter(|task| matches!(task.status.as_str(), "completed" | "skipped"))
        .count();
    let active = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status.as_str(),
                "running" | "queued" | "paused" | "pausing" | "cancelling"
            )
        })
        .count();
    let blocked_tasks = tasks.iter().filter(|task| task.status == "blocked").count();
    let failed_tasks = tasks.iter().filter(|task| task.status == "failed").count();
    let retrying = tasks
        .iter()
        .filter(|task| task.next_attempt_at.is_some())
        .count();
    let attention = failed_agents
        .saturating_add(failed_tasks)
        .saturating_add(blocked_tasks)
        .saturating_add(merge_conflicts)
        .saturating_add(open_notes.len());
    let metrics = OrchestrationMetrics {
        total_agents: input.agents.len(),
        running_agents: input
            .agents
            .iter()
            .filter(|agent| matches!(agent.status.as_str(), "running" | "verifying" | "merging"))
            .count(),
        queued_agents: input
            .agents
            .iter()
            .filter(|agent| agent.status == "queued")
            .count(),
        failed_agents,
        total_tasks: tasks.len(),
        completed_tasks: completed,
        active_tasks: active,
        ready_tasks: ready,
        blocked_tasks,
        failed_tasks,
        retrying_tasks: retrying,
        open_blockers: open_notes.len(),
        merge_queue_depth: input.merge_queue.len(),
        merge_conflicts,
        attention_count: attention,
    };

    OrchestrationSnapshot {
        session_id: input.session.id.clone(),
        project_id: input.session.project_id.clone(),
        goal: input.session.goal.clone(),
        status: input.session.status.clone(),
        plan: plan.cloned(),
        plan_revision: revision,
        policy: input.policy.clone(),
        departments,
        tasks,
        agents: input.agents.to_vec(),
        notes: notes.to_vec(),
        decisions: input.decisions.to_vec(),
        revisions,
        blockers,
        metrics,
        generated_at: now_rfc3339(),
    }
}

fn owner_agents(agents: &[AgentRecord]) -> HashMap<&str, &AgentRecord> {
    let mut owners = HashMap::new();
    for agent in agents
        .iter()
        .filter(|agent| matches!(agent.role.as_str(), "worker" | "labour" | "verifier"))
    {
        if let Some(task_id) = agent.task_id.as_deref() {
            owners.entry(task_id).or_insert(agent);
        }
    }
    owners
}

fn verifier_status_by_task(agents: &[AgentRecord], events: &[Event]) -> HashMap<String, String> {
    let agent_task = agents
        .iter()
        .filter(|agent| agent.role == "verifier")
        .filter_map(|agent| {
            agent
                .task_id
                .as_deref()
                .map(|task| (agent.id.as_str(), task))
        })
        .collect::<HashMap<_, _>>();
    let mut statuses = HashMap::new();
    for event in events.iter().rev() {
        if event.kind != "agent.finished" {
            continue;
        }
        let Some(agent_id) = event
            .payload
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(task_id) = agent_task.get(agent_id) else {
            continue;
        };
        let Some(approved) = event
            .payload
            .get("approved")
            .and_then(serde_json::Value::as_bool)
        else {
            continue;
        };
        statuses
            .entry(task_id.to_string())
            .or_insert_with(|| if approved { "approved" } else { "rejected" }.to_string());
    }
    statuses
}

pub fn decisions_from_events(events: &[Event]) -> Vec<OrchestrationDecision> {
    events
        .iter()
        .filter(|event| event.kind == "orchestration.decision")
        .map(|event| OrchestrationDecision {
            decision_id: event
                .payload
                .get("decision_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("decision-{}", event.seq)),
            action: event
                .payload
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            task_id: event
                .payload
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            detail: event
                .payload
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            attempt: event
                .payload
                .get("attempt")
                .and_then(serde_json::Value::as_u64)
                .map(|attempt| attempt as u32)
                .unwrap_or(0),
            agent_id: event
                .payload
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("orchestrator")
                .to_string(),
            created_at: event.created_at.clone(),
        })
        .collect()
}

pub fn coordination_notes(events: &[Event]) -> Vec<OrchestrationNote> {
    let mut notes = Vec::<OrchestrationNote>::new();
    let mut index_by_task = HashMap::<String, Vec<usize>>::new();
    let mut index_by_note = HashMap::<String, usize>::new();
    for event in events {
        if event.kind != "coordination.note" {
            continue;
        }
        let note_id = event
            .payload
            .get("note_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("note-{}", event.seq));
        let action = event
            .payload
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("note")
            .to_string();
        let task_id = event
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        if action == "unblock" {
            let target_note = event
                .payload
                .get("target_note_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|note_id| index_by_note.get(note_id).copied());
            if let Some(index) = target_note {
                notes[index].status = "resolved".to_string();
                notes[index].resolved_at = Some(event.created_at.clone());
            }
            if let Some(task_id) = &task_id
                && let Some(indices) = index_by_task.remove(task_id)
            {
                for index in indices {
                    notes[index].status = "resolved".to_string();
                    notes[index].resolved_at = Some(event.created_at.clone());
                }
            }
            continue;
        }
        let note = OrchestrationNote {
            note_id: note_id.clone(),
            action: action.clone(),
            severity: event
                .payload
                .get("severity")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("info")
                .to_string(),
            status: if action == "blocker" {
                "open".to_string()
            } else {
                "info".to_string()
            },
            task_id: task_id.clone(),
            target_agent_id: event
                .payload
                .get("target_agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            agent_id: event
                .payload
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
            agent_name: event
                .payload
                .get("agent_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("agent")
                .to_string(),
            message: event
                .payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            created_at: event.created_at.clone(),
            resolved_at: None,
        };
        let index = notes.len();
        notes.push(note);
        index_by_note.insert(note_id, index);
        if action == "blocker"
            && let Some(task_id) = task_id
        {
            index_by_task.entry(task_id).or_default().push(index);
        }
    }
    notes
}

pub fn task_ids(plan: &TeamPlan) -> HashSet<String> {
    plan.tasks.iter().map(|task| task.id.clone()).collect()
}

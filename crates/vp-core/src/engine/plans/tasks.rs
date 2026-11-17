use super::*;

impl Engine {
    pub async fn task_queue(
        &self,
        project_id: Option<String>,
        session_id: Option<String>,
    ) -> Result<TaskQueueResult> {
        self.scheduler.queue(project_id, session_id).await
    }

    pub async fn retry_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.retry_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn skip_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.skip_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn request_plan_task_cancel(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.request_plan_task_cancel_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn pause_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.pause_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn mark_plan_task_paused(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<ScheduledTask>> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let result = tokio::task::spawn_blocking(move || {
            store.mark_plan_task_paused_with_event(&session_id, &task_id)
        })
        .await??;
        let Some((task, event)) = result else {
            return Ok(None);
        };
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(Some(task))
    }

    pub async fn resume_plan_task(&self, session_id: &str, task_id: &str) -> Result<ScheduledTask> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let (task, event) = tokio::task::spawn_blocking(move || {
            store.resume_plan_task_with_event(&session_id, &task_id)
        })
        .await??;
        let _ = self.events.send(event);
        self.scheduler.notify_state();
        Ok(task)
    }

    pub async fn plan_task_status(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<String>> {
        self.scheduler.plan_task_status(session_id, task_id).await
    }

    pub async fn plan_task_cancel_requested(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<bool> {
        let store = self.store.clone();
        let session_id = session_id.to_string();
        let task_id = task_id.to_string();
        let status =
            tokio::task::spawn_blocking(move || store.plan_task_status(&session_id, &task_id))
                .await??;
        Ok(matches!(
            status.as_deref(),
            Some("cancelling" | "cancelled")
        ))
    }

    pub async fn interrupt_running_plan_tasks(&self, session_id: &str) -> Result<usize> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let tasks = tokio::task::spawn_blocking(move || {
            store.interrupt_session_plan_tasks(&id, "the session ended before the task finished")
        })
        .await??;
        let count = tasks.len();
        for task in tasks {
            self.append_event(
                session_id,
                "plan_task.interrupted",
                serde_json::json!({
                    "task_id": task.task_id,
                    "task_status": "interrupted",
                    "reason": "the session ended before the task finished",
                }),
            )
            .await?;
        }
        Ok(count)
    }

    pub async fn set_todos(&self, session_id: &str, items: Vec<TodoItem>) -> Result<()> {
        let store = self.store.clone();
        let id = session_id.to_string();
        let items_for_store = items.clone();
        let event =
            tokio::task::spawn_blocking(move || store.set_todos_with_event(&id, &items_for_store))
                .await??;
        let _ = self.events.send(event);
        Ok(())
    }

    pub async fn list_todos(&self, session_id: &str) -> Result<Vec<TodoItem>> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_todos(&id)).await?
    }
}

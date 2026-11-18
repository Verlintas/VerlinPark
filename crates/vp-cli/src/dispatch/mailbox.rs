//! Session mailbox and mailbox-policy commands.

use super::*;

pub(super) async fn dispatch(
    command: Command,
    discovery: Discovery,
    _json: bool,
) -> Result<Option<Value>> {
    let value = match command {
        Command::Mailbox {
            session,
            agent,
            unread,
        } => {
            rpc(
                &discovery,
                "mailbox.list",
                json!({
                    "session_id": session,
                    "agent_id": agent,
                    "unread_only": unread,
                }),
            )
            .await?
        }
        Command::ReadMessage { message } => {
            rpc(&discovery, "mailbox.read", json!({"message_id": message})).await?
        }
        Command::MailboxPolicy { project } => {
            rpc(
                &discovery,
                "mailbox.policy.get",
                json!({"project_id": project}),
            )
            .await?
        }
        Command::MailboxPolicySet {
            project,
            max_age_secs,
            max_count,
            delete_unread,
            auto_gc,
        } => {
            rpc(
                &discovery,
                "mailbox.policy.set",
                json!({
                    "project_id": project,
                    "policy": {
                        "max_age_secs": max_age_secs,
                        "max_count": max_count,
                        "delete_unread": delete_unread,
                        "auto_gc": auto_gc,
                    }
                }),
            )
            .await?
        }
        Command::MailboxGc { project } => {
            rpc(&discovery, "mailbox.gc", json!({"project_id": project})).await?
        }
        _ => unreachable!("command routed to the wrong dispatch family"),
    };
    Ok(Some(value))
}

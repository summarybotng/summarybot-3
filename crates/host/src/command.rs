//! Command execution (ADR-122) — host side of the platform-agnostic surface.
//!
//! A platform adapter parses an invocation into a [`domain::Command`]
//! ([`domain::parse_command`]) then calls [`execute_command`], which reuses the
//! existing schedule store / summarize pipeline — the same service path as the
//! dashboard and scheduled runs (CMD-001: one capability, many triggers). The
//! reply is platform-agnostic; the adapter renders it (Discord ephemeral, Slack
//! response_url, …). Long-running summaries defer (CMD: ack now, deliver later).

use domain::command::{Command, CommandScope};
use domain::WorkspaceId;
use repository::ScheduleRepository;

/// A platform-agnostic command reply the adapter renders natively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandReply {
    pub text: String,
    /// Private-by-default where the platform supports it (ADR-122 §5).
    pub ephemeral: bool,
    /// The work runs async and will be delivered when ready (ADR-122 §3): the
    /// adapter should ack with a "deferred"/typing affordance.
    pub deferred: bool,
}

impl CommandReply {
    fn ephemeral(text: impl Into<String>) -> Self {
        Self { text: text.into(), ephemeral: true, deferred: false }
    }
    fn deferred(text: impl Into<String>) -> Self {
        Self { text: text.into(), ephemeral: true, deferred: true }
    }
}

/// Execute the schedule-management commands against the store and shape the
/// summarize command into a deferred ack. Permission checks + audit live in the
/// API boundary (PRM-005/AUD-001); this is the pure-ish execution step over the
/// repository.
pub fn execute_command<R: ScheduleRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    cmd: &Command,
) -> anyhow::Result<CommandReply> {
    Ok(match cmd {
        Command::Summarize { scope, lookback_secs, .. } => {
            // The run reuses the Phase-3 pipeline via the adapter; here we ack.
            let where_ = match scope {
                CommandScope::Channel(c) => format!("#{c}"),
                CommandScope::Category(c) => format!("category {c}"),
                CommandScope::Workspace => "all channels".to_string(),
            };
            let hours = (*lookback_secs / 3_600).max(1);
            CommandReply::deferred(format!(
                "Summarizing {where_} over the last {hours}h — I'll post the summary here when it's ready."
            ))
        }
        Command::ScheduleList => {
            let schedules = repo.list_for_workspace(workspace)?;
            if schedules.is_empty() {
                CommandReply::ephemeral("No schedules in this workspace.")
            } else {
                let mut out = String::from("**Schedules:**\n");
                for s in schedules {
                    out.push_str(&format!(
                        "- `{}` — {} · {} · next run {}\n",
                        s.id,
                        format!("{:?}", s.schedule.schedule_type),
                        if s.schedule.enabled { "enabled" } else { "paused" },
                        s.next_run,
                    ));
                }
                CommandReply::ephemeral(out)
            }
        }
        Command::SchedulePause(id) => set_enabled(repo, workspace, id, false)?,
        Command::ScheduleResume(id) => set_enabled(repo, workspace, id, true)?,
        Command::ScheduleDelete(id) => {
            if repo.delete_schedule(workspace, id)? {
                CommandReply::ephemeral(format!("Deleted schedule `{id}`."))
            } else {
                CommandReply::ephemeral(format!("No schedule `{id}` in this workspace."))
            }
        }
        Command::ScheduleStatus(id) => match repo.get_schedule(workspace, id)? {
            Some(s) => CommandReply::ephemeral(format!(
                "`{}` — {} · {} · {} consecutive failures · next run {}",
                s.id,
                format!("{:?}", s.schedule.schedule_type),
                if s.schedule.enabled { "enabled" } else { "paused" },
                s.consecutive_failures,
                s.next_run,
            )),
            None => CommandReply::ephemeral(format!("No schedule `{id}` in this workspace.")),
        },
    })
}

fn set_enabled<R: ScheduleRepository>(
    repo: &R,
    workspace: &WorkspaceId,
    id: &str,
    enabled: bool,
) -> anyhow::Result<CommandReply> {
    let verb = if enabled { "Resumed" } else { "Paused" };
    Ok(if repo.set_enabled(workspace, id, enabled)? {
        CommandReply::ephemeral(format!("{verb} schedule `{id}`."))
    } else {
        CommandReply::ephemeral(format!("No schedule `{id}` in this workspace."))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{Schedule, WorkspaceId};
    use repository::{SqliteRepository, StoredSchedule};

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    fn seed(repo: &SqliteRepository, id: &str) {
        let schedule = Schedule::build(
            ws(), "daily", 9, 0, &[], 1, "UTC", None, 0, true, Some("c1"), 86_400,
        )
        .unwrap();
        repo.create_schedule(&StoredSchedule {
            id: id.into(),
            schedule,
            next_run: 1000,
            consecutive_failures: 0,
        })
        .unwrap();
    }

    #[test]
    fn summarize_command_defers() {
        let repo = SqliteRepository::in_memory().unwrap();
        let cmd = Command::Summarize {
            scope: CommandScope::Workspace,
            lookback_secs: 86_400,
            length: domain::summarize::SummaryLength::Brief,
        };
        let r = execute_command(&repo, &ws(), &cmd).unwrap();
        assert!(r.deferred);
        assert!(r.ephemeral);
        assert!(r.text.contains("all channels"));
    }

    #[test]
    fn schedule_list_pause_resume_delete_status() {
        let repo = SqliteRepository::in_memory().unwrap();
        seed(&repo, "sch_1");

        assert!(execute_command(&repo, &ws(), &Command::ScheduleList).unwrap().text.contains("sch_1"));

        // Pause → the store reflects it; status reports paused.
        execute_command(&repo, &ws(), &Command::SchedulePause("sch_1".into())).unwrap();
        let st = execute_command(&repo, &ws(), &Command::ScheduleStatus("sch_1".into())).unwrap();
        assert!(st.text.contains("paused"));

        execute_command(&repo, &ws(), &Command::ScheduleResume("sch_1".into())).unwrap();
        let st = execute_command(&repo, &ws(), &Command::ScheduleStatus("sch_1".into())).unwrap();
        assert!(st.text.contains("enabled"));

        // Delete, then status/delete report "no schedule".
        assert!(execute_command(&repo, &ws(), &Command::ScheduleDelete("sch_1".into())).unwrap().text.contains("Deleted"));
        assert!(execute_command(&repo, &ws(), &Command::ScheduleStatus("sch_1".into())).unwrap().text.contains("No schedule"));
        assert!(execute_command(&repo, &ws(), &Command::ScheduleDelete("nope".into())).unwrap().text.contains("No schedule"));
    }
}

//! Platform-agnostic command surface (ADR-122; CMD-001..006) — pure core.
//!
//! Commands are defined **once, abstractly** here ("summarize this scope over
//! this range", "manage these schedules"). Each platform adapter renders them
//! natively (Discord application commands, Slack slash commands) and parses an
//! invocation into a [`Command`] via [`parse_command`]; no platform logic leaks
//! into core (WSP-006). Execution reuses the same summarize/schedule services as
//! the dashboard and scheduled runs — this is just a different trigger.

use crate::summarize::SummaryLength;

/// The channel scope a `/summarize` targets (mirrors ADR-011 schedule scope).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandScope {
    /// One channel/chat by id.
    Channel(String),
    /// All channels under a platform category id (Discord).
    Category(String),
    /// All of the workspace's channels.
    Workspace,
}

/// A parsed, platform-agnostic command (ADR-122).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// On-demand summary (ODS-001): a scope, a lookback window, a length.
    Summarize {
        scope: CommandScope,
        lookback_secs: i64,
        length: SummaryLength,
    },
    /// `/schedule list` — the workspace's schedules.
    ScheduleList,
    /// `/schedule pause|resume|delete|status <id>`.
    SchedulePause(String),
    ScheduleResume(String),
    ScheduleDelete(String),
    ScheduleStatus(String),
}

/// Why an invocation couldn't be parsed into a [`Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// No such command / subcommand.
    Unknown(String),
    /// A required argument was absent.
    MissingArg(&'static str),
    /// An argument had an unrecognized value.
    BadArg { arg: &'static str, value: String },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Unknown(c) => write!(f, "unknown command: {c}"),
            CommandError::MissingArg(a) => write!(f, "missing required argument: {a}"),
            CommandError::BadArg { arg, value } => write!(f, "invalid {arg}: {value}"),
        }
    }
}

/// Named arguments from an invocation, looked up by name (the adapter pulls these
/// from the platform's option list — Discord options, Slack parsed text, etc.).
pub trait CommandArgs {
    fn get(&self, name: &str) -> Option<&str>;
}

impl CommandArgs for std::collections::HashMap<String, String> {
    fn get(&self, name: &str) -> Option<&str> {
        self.get(name).map(String::as_str)
    }
}

/// Parse a lookback like `24h`, `7d`, `90m`, `3600s` (defaults to seconds) into
/// seconds. Used by `/summarize range:`.
pub fn parse_lookback(raw: &str) -> Option<i64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let (num, unit) = raw.split_at(raw.find(|c: char| !c.is_ascii_digit()).unwrap_or(raw.len()));
    let n: i64 = num.parse().ok()?;
    let mult = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        _ => return None,
    };
    n.checked_mul(mult).filter(|s| *s > 0)
}

fn parse_length(raw: &str) -> Option<SummaryLength> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "brief" | "short" => Some(SummaryLength::Brief),
        "detailed" | "normal" | "" => Some(SummaryLength::Detailed),
        "comprehensive" | "long" | "full" => Some(SummaryLength::Comprehensive),
        _ => None,
    }
}

/// Parse a `(command, subcommand, args)` invocation into a [`Command`] (CMD-001).
/// `subcommand` is `None` for flat commands like `summarize`. Defaults: range
/// 24h, length detailed, scope = the channel the command was invoked in (passed
/// as `args["channel"]`); `scope:all` / `category:<id>` widen it.
pub fn parse_command(
    command: &str,
    subcommand: Option<&str>,
    args: &impl CommandArgs,
) -> Result<Command, CommandError> {
    match command {
        "summarize" => {
            let length = match args.get("length") {
                Some(v) => parse_length(v).ok_or(CommandError::BadArg { arg: "length", value: v.to_string() })?,
                None => SummaryLength::Detailed,
            };
            let lookback = match args.get("range") {
                Some(v) => parse_lookback(v).ok_or(CommandError::BadArg { arg: "range", value: v.to_string() })?,
                None => 24 * 3_600,
            };
            // Scope precedence: explicit category, then "all", then a channel id.
            let scope = if let Some(cat) = args.get("category").filter(|c| !c.is_empty()) {
                CommandScope::Category(cat.to_string())
            } else if args.get("scope").map(|s| s.eq_ignore_ascii_case("all")).unwrap_or(false) {
                CommandScope::Workspace
            } else if let Some(ch) = args.get("channel").filter(|c| !c.is_empty()) {
                CommandScope::Channel(ch.to_string())
            } else {
                return Err(CommandError::MissingArg("channel"));
            };
            Ok(Command::Summarize { scope, lookback_secs: lookback, length })
        }
        "schedule" => {
            let id = || args.get("id").filter(|s| !s.is_empty()).map(str::to_string).ok_or(CommandError::MissingArg("id"));
            match subcommand {
                Some("list") | None => Ok(Command::ScheduleList),
                Some("pause") => Ok(Command::SchedulePause(id()?)),
                Some("resume") => Ok(Command::ScheduleResume(id()?)),
                Some("delete") => Ok(Command::ScheduleDelete(id()?)),
                Some("status") => Ok(Command::ScheduleStatus(id()?)),
                Some(other) => Err(CommandError::Unknown(format!("schedule {other}"))),
            }
        }
        other => Err(CommandError::Unknown(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn args(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn lookback_units() {
        assert_eq!(parse_lookback("24h"), Some(86_400));
        assert_eq!(parse_lookback("7d"), Some(604_800));
        assert_eq!(parse_lookback("90m"), Some(5_400));
        assert_eq!(parse_lookback("3600"), Some(3_600)); // bare = seconds
        assert_eq!(parse_lookback("0h"), None);
        assert_eq!(parse_lookback("nope"), None);
    }

    #[test]
    fn summarize_defaults_and_scope_precedence() {
        // Defaults: 24h, detailed; channel scope from the invoking channel.
        let c = parse_command("summarize", None, &args(&[("channel", "c1")])).unwrap();
        assert_eq!(
            c,
            Command::Summarize {
                scope: CommandScope::Channel("c1".into()),
                lookback_secs: 86_400,
                length: SummaryLength::Detailed,
            }
        );
        // scope:all widens to the workspace; range + length honored.
        let c = parse_command(
            "summarize",
            None,
            &args(&[("channel", "c1"), ("scope", "all"), ("range", "7d"), ("length", "brief")]),
        )
        .unwrap();
        assert_eq!(
            c,
            Command::Summarize {
                scope: CommandScope::Workspace,
                lookback_secs: 604_800,
                length: SummaryLength::Brief,
            }
        );
        // category beats channel.
        let c = parse_command("summarize", None, &args(&[("channel", "c1"), ("category", "cat9")])).unwrap();
        assert!(matches!(c, Command::Summarize { scope: CommandScope::Category(c), .. } if c == "cat9"));
    }

    #[test]
    fn summarize_needs_a_scope_and_validates_args() {
        assert_eq!(parse_command("summarize", None, &args(&[])), Err(CommandError::MissingArg("channel")));
        assert_eq!(
            parse_command("summarize", None, &args(&[("channel", "c1"), ("range", "5x")])),
            Err(CommandError::BadArg { arg: "range", value: "5x".into() })
        );
    }

    #[test]
    fn schedule_subcommands() {
        assert_eq!(parse_command("schedule", Some("list"), &args(&[])).unwrap(), Command::ScheduleList);
        assert_eq!(
            parse_command("schedule", Some("pause"), &args(&[("id", "sch_1")])).unwrap(),
            Command::SchedulePause("sch_1".into())
        );
        assert_eq!(
            parse_command("schedule", Some("delete"), &args(&[])),
            Err(CommandError::MissingArg("id"))
        );
        assert_eq!(
            parse_command("schedule", Some("frobnicate"), &args(&[])),
            Err(CommandError::Unknown("schedule frobnicate".into()))
        );
    }

    #[test]
    fn unknown_command_is_rejected() {
        assert_eq!(parse_command("dance", None, &args(&[])), Err(CommandError::Unknown("dance".into())));
    }
}

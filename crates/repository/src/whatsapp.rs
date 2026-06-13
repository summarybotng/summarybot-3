//! WhatsApp ingestion persistence (PRD §2.3; ADR-121).
//!
//! Three tables behind the collection pipeline:
//!   * `whatsapp_imports` — attributed import records with file-hash dedup
//!     (WHA-009/010);
//!   * `whatsapp_participants` — resolved per-chat identities (phone-hash +
//!     aliases) so the same person collapses across exports (WHA-013);
//!   * `messages` — normalized messages keyed by their fingerprint id, so a
//!     re-import is idempotent (WHA-012): inserting a known id is a no-op.
//!
//! Anonymization, fingerprinting and the resolution *policy* are host-side
//! (ADR-121); this is the storage they sit on.

use crate::SqliteRepository;
use anyhow::Result;
use domain::{
    Attachment, AttachmentKind, ChannelId, MessageId, NormalizedMessage, Platform, UserId,
    WorkspaceId,
};
use rusqlite::{params, OptionalExtension};

/// Outcome of recording an import (file-hash dedup, WHA-010).
#[derive(Debug, PartialEq, Eq)]
pub enum ImportOutcome {
    /// First time this file was seen for the (workspace, chat) — newly recorded.
    Recorded,
    /// An identical file was already imported; the existing import id is returned.
    Duplicate(String),
}

/// A resolved per-chat participant (WHA-013).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    pub id: String,
    pub pseudonym: String,
}

/// Metadata for an import record (WHA-009).
#[derive(Debug, Clone)]
pub struct ImportRecord<'a> {
    pub id: &'a str,
    pub workspace_id: &'a WorkspaceId,
    pub chat_id: &'a ChannelId,
    pub file_hash: &'a str,
    pub uploader: &'a UserId,
    pub imported_at: i64,
    pub format: &'a str,
    pub message_count: i64,
    pub date_start: i64,
    pub date_end: i64,
    /// Detected group-creation instant, if a creation/added system message was
    /// found in this export (WHA-015). Anchors `before_join` coverage gaps.
    pub group_created_at: Option<i64>,
}

/// A stored import, read back for coverage analysis (WHA-016).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredImport {
    pub id: String,
    pub uploader: String,
    pub imported_at: i64,
    pub message_count: i64,
    pub date_start: i64,
    pub date_end: i64,
    pub group_created_at: Option<i64>,
}

/// Per-chat rollup for the coverage overview (WHA-017): how many imports, total
/// messages, and the covered span endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSummary {
    pub chat_id: String,
    pub import_count: i64,
    pub message_count: i64,
    pub earliest: i64,
    pub latest: i64,
}

/// Storage boundary for the WhatsApp collection pipeline.
pub trait WhatsAppRepository {
    /// Record an import, rejecting a re-uploaded identical file (WHA-010).
    fn record_import(&self, rec: &ImportRecord) -> Result<ImportOutcome>;
    /// A chat's imports, oldest first — the spans fed to coverage analysis.
    fn list_imports(&self, workspace: &WorkspaceId, chat: &ChannelId) -> Result<Vec<StoredImport>>;
    /// All chats with at least one import in the workspace, with a coverage rollup.
    fn list_chats(&self, workspace: &WorkspaceId) -> Result<Vec<ChatSummary>>;
    /// Find the participant bound to a phone hash in a chat, if any.
    fn participant_by_phone(
        &self,
        workspace: &WorkspaceId,
        chat: &ChannelId,
        phone_hash: &str,
    ) -> Result<Option<Participant>>;
    /// Find the participant carrying `alias` in a chat, if any.
    fn participant_by_alias(
        &self,
        workspace: &WorkspaceId,
        chat: &ChannelId,
        alias: &str,
    ) -> Result<Option<Participant>>;
    /// Create a participant. `phone_hash`/`alias` seed the resolution indexes.
    fn create_participant(
        &self,
        workspace: &WorkspaceId,
        chat: &ChannelId,
        participant: &Participant,
        phone_hash: Option<&str>,
        alias: Option<&str>,
    ) -> Result<()>;
    /// Add an alias to an existing participant (idempotent).
    fn add_alias(&self, participant_id: &str, alias: &str) -> Result<()>;
    /// Persist a normalized message. Returns `true` if newly stored, `false` if
    /// its fingerprint id was already present (WHA-012 idempotent re-ingest).
    fn save_message(&self, workspace: &WorkspaceId, message: &NormalizedMessage) -> Result<bool>;
    /// Read a stored message back (used by the Phase 3 summarization pipeline),
    /// scoped to its workspace.
    fn get_message(
        &self,
        workspace: &WorkspaceId,
        id: &MessageId,
    ) -> Result<Option<NormalizedMessage>>;
    /// List a channel's messages in the inclusive UTC range `[start, end]`,
    /// oldest first — what the summarizer reads for a scope/period.
    fn list_messages(
        &self,
        workspace: &WorkspaceId,
        channel: &ChannelId,
        start: i64,
        end: i64,
    ) -> Result<Vec<NormalizedMessage>>;
}

impl WhatsAppRepository for SqliteRepository {
    fn record_import(&self, rec: &ImportRecord) -> Result<ImportOutcome> {
        let existing: Option<String> = self
            .conn
            .query_row(
                "SELECT id FROM whatsapp_imports
                 WHERE workspace_id = ?1 AND chat_id = ?2 AND file_hash = ?3",
                params![
                    rec.workspace_id.as_str(),
                    rec.chat_id.as_str(),
                    rec.file_hash
                ],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            return Ok(ImportOutcome::Duplicate(id));
        }
        self.conn.execute(
            "INSERT INTO whatsapp_imports
               (id, workspace_id, chat_id, file_hash, uploader, imported_at,
                format, message_count, date_start, date_end, group_created_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                rec.id,
                rec.workspace_id.as_str(),
                rec.chat_id.as_str(),
                rec.file_hash,
                rec.uploader.as_str(),
                rec.imported_at,
                rec.format,
                rec.message_count,
                rec.date_start,
                rec.date_end,
                rec.group_created_at,
            ],
        )?;
        Ok(ImportOutcome::Recorded)
    }

    fn list_imports(&self, workspace: &WorkspaceId, chat: &ChannelId) -> Result<Vec<StoredImport>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, uploader, imported_at, message_count, date_start, date_end, group_created_at
             FROM whatsapp_imports WHERE workspace_id = ?1 AND chat_id = ?2
             ORDER BY date_start, imported_at",
        )?;
        let rows = stmt.query_map(params![workspace.as_str(), chat.as_str()], |row| {
            Ok(StoredImport {
                id: row.get(0)?,
                uploader: row.get(1)?,
                imported_at: row.get(2)?,
                message_count: row.get(3)?,
                date_start: row.get(4)?,
                date_end: row.get(5)?,
                group_created_at: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    fn list_chats(&self, workspace: &WorkspaceId) -> Result<Vec<ChatSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT chat_id, COUNT(*), COALESCE(SUM(message_count),0),
                    MIN(date_start), MAX(date_end)
             FROM whatsapp_imports WHERE workspace_id = ?1
             GROUP BY chat_id ORDER BY chat_id",
        )?;
        let rows = stmt.query_map(params![workspace.as_str()], |row| {
            Ok(ChatSummary {
                chat_id: row.get(0)?,
                import_count: row.get(1)?,
                message_count: row.get(2)?,
                earliest: row.get(3)?,
                latest: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    fn participant_by_phone(
        &self,
        workspace: &WorkspaceId,
        chat: &ChannelId,
        phone_hash: &str,
    ) -> Result<Option<Participant>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, pseudonym FROM whatsapp_participants
                 WHERE workspace_id = ?1 AND chat_id = ?2 AND phone_hash = ?3",
                params![workspace.as_str(), chat.as_str(), phone_hash],
                |row| {
                    Ok(Participant {
                        id: row.get(0)?,
                        pseudonym: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    fn participant_by_alias(
        &self,
        workspace: &WorkspaceId,
        chat: &ChannelId,
        alias: &str,
    ) -> Result<Option<Participant>> {
        // Aliases are stored newline-delimited; match a whole-line entry.
        Ok(self
            .conn
            .query_row(
                "SELECT id, pseudonym FROM whatsapp_participants
                 WHERE workspace_id = ?1 AND chat_id = ?2
                   AND instr(char(10) || aliases || char(10), char(10) || ?3 || char(10)) > 0",
                params![workspace.as_str(), chat.as_str(), alias],
                |row| {
                    Ok(Participant {
                        id: row.get(0)?,
                        pseudonym: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    fn create_participant(
        &self,
        workspace: &WorkspaceId,
        chat: &ChannelId,
        participant: &Participant,
        phone_hash: Option<&str>,
        alias: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO whatsapp_participants
               (id, workspace_id, chat_id, phone_hash, pseudonym, aliases)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                participant.id,
                workspace.as_str(),
                chat.as_str(),
                phone_hash,
                participant.pseudonym,
                alias.unwrap_or(""),
            ],
        )?;
        Ok(())
    }

    fn add_alias(&self, participant_id: &str, alias: &str) -> Result<()> {
        let current: Option<String> = self
            .conn
            .query_row(
                "SELECT aliases FROM whatsapp_participants WHERE id = ?1",
                params![participant_id],
                |row| row.get(0),
            )
            .optional()?;
        let current = current.unwrap_or_default();
        if current.lines().any(|a| a == alias) {
            return Ok(());
        }
        let updated = if current.is_empty() {
            alias.to_string()
        } else {
            format!("{current}\n{alias}")
        };
        self.conn.execute(
            "UPDATE whatsapp_participants SET aliases = ?2 WHERE id = ?1",
            params![participant_id, updated],
        )?;
        Ok(())
    }

    fn save_message(&self, workspace: &WorkspaceId, message: &NormalizedMessage) -> Result<bool> {
        let (att_kind, att_name) = match message.attachments.first() {
            Some(a) => (Some(attachment_kind_str(a.kind)), a.filename.clone()),
            None => (None, None),
        };
        let changed = self.conn.execute(
            "INSERT OR IGNORE INTO messages
               (id, workspace_id, platform, channel_id, author_id, author_name,
                content, timestamp, is_system, reply_to, attachment_kind, attachment_name)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                message.id.as_str(),
                workspace.as_str(),
                message.platform.as_str(),
                message.channel_id.as_str(),
                message.author_id,
                message.author_name,
                message.content,
                message.timestamp,
                message.is_system,
                message.reply_to.as_ref().map(|r| r.as_str()),
                att_kind,
                att_name,
            ],
        )?;
        Ok(changed > 0)
    }

    fn get_message(
        &self,
        workspace: &WorkspaceId,
        id: &MessageId,
    ) -> Result<Option<NormalizedMessage>> {
        self.conn
            .query_row(
                "SELECT platform, channel_id, author_id, author_name, content, timestamp,
                        is_system, reply_to, attachment_kind, attachment_name
                 FROM messages WHERE id = ?1 AND workspace_id = ?2",
                params![id.as_str(), workspace.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<String>>(8)?,
                        row.get::<_, Option<String>>(9)?,
                    ))
                },
            )
            .optional()?
            .map(
                |(platform, channel, author_id, author_name, content, ts, sys, reply, ak, an)| {
                    let attachments = ak
                        .map(|k| {
                            vec![Attachment {
                                kind: parse_attachment_kind(&k),
                                filename: an,
                            }]
                        })
                        .unwrap_or_default();
                    Ok(NormalizedMessage {
                        id: id.clone(),
                        platform: Platform::parse(&platform).map_err(anyhow::Error::new)?,
                        channel_id: ChannelId::parse(channel).map_err(anyhow::Error::new)?,
                        author_id,
                        author_name,
                        content,
                        timestamp: ts,
                        is_system: sys,
                        reply_to: reply
                            .map(MessageId::parse)
                            .transpose()
                            .map_err(anyhow::Error::new)?,
                        attachments,
                    })
                },
            )
            .transpose()
    }

    fn list_messages(
        &self,
        workspace: &WorkspaceId,
        channel: &ChannelId,
        start: i64,
        end: i64,
    ) -> Result<Vec<NormalizedMessage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, platform, author_id, author_name, content, timestamp,
                    is_system, reply_to, attachment_kind, attachment_name
             FROM messages
             WHERE workspace_id = ?1 AND channel_id = ?2 AND timestamp BETWEEN ?3 AND ?4
             ORDER BY timestamp",
        )?;
        let rows = stmt.query_map(
            params![workspace.as_str(), channel.as_str(), start, end],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                ))
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            let (id, platform, author_id, author_name, content, ts, sys, reply, ak, an) = row?;
            let attachments = ak
                .map(|k| {
                    vec![Attachment {
                        kind: parse_attachment_kind(&k),
                        filename: an,
                    }]
                })
                .unwrap_or_default();
            out.push(NormalizedMessage {
                id: MessageId::parse(id).map_err(anyhow::Error::new)?,
                platform: Platform::parse(&platform).map_err(anyhow::Error::new)?,
                channel_id: channel.clone(),
                author_id,
                author_name,
                content,
                timestamp: ts,
                is_system: sys,
                reply_to: reply
                    .map(MessageId::parse)
                    .transpose()
                    .map_err(anyhow::Error::new)?,
                attachments,
            });
        }
        Ok(out)
    }
}

fn attachment_kind_str(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Video => "video",
        AttachmentKind::Audio => "audio",
        AttachmentKind::Document => "document",
        AttachmentKind::Other => "other",
    }
}

fn parse_attachment_kind(s: &str) -> AttachmentKind {
    match s {
        "image" => AttachmentKind::Image,
        "video" => AttachmentKind::Video,
        "audio" => AttachmentKind::Audio,
        "document" => AttachmentKind::Document,
        _ => AttachmentKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }
    fn chat() -> ChannelId {
        ChannelId::parse("chat-1").unwrap()
    }

    fn import_rec<'a>(
        id: &'a str,
        file_hash: &'a str,
        ws: &'a WorkspaceId,
        chat: &'a ChannelId,
        uploader: &'a UserId,
    ) -> ImportRecord<'a> {
        ImportRecord {
            id,
            workspace_id: ws,
            chat_id: chat,
            file_hash,
            uploader,
            imported_at: 1_000,
            format: "ios",
            message_count: 10,
            date_start: 0,
            date_end: 100,
            group_created_at: None,
        }
    }

    #[test]
    fn identical_file_is_a_duplicate_import() {
        let repo = repo();
        let (ws, chat, up) = (ws(), chat(), UserId::parse("u1").unwrap());
        assert_eq!(
            repo.record_import(&import_rec("imp-1", "hashA", &ws, &chat, &up))
                .unwrap(),
            ImportOutcome::Recorded
        );
        assert_eq!(
            repo.record_import(&import_rec("imp-2", "hashA", &ws, &chat, &up))
                .unwrap(),
            ImportOutcome::Duplicate("imp-1".to_string())
        );
    }

    #[test]
    fn participant_resolved_by_phone_then_alias() {
        let repo = repo();
        let (ws, chat) = (ws(), chat());
        let p = Participant {
            id: "p1".into(),
            pseudonym: "Swift-Penguin".into(),
        };
        repo.create_participant(&ws, &chat, &p, Some("phash"), Some("Rob Smith"))
            .unwrap();

        assert_eq!(
            repo.participant_by_phone(&ws, &chat, "phash")
                .unwrap()
                .unwrap()
                .id,
            "p1"
        );
        assert_eq!(
            repo.participant_by_alias(&ws, &chat, "Rob Smith")
                .unwrap()
                .unwrap()
                .id,
            "p1"
        );
        assert!(repo
            .participant_by_alias(&ws, &chat, "Unknown")
            .unwrap()
            .is_none());
    }

    #[test]
    fn add_alias_is_idempotent_and_matches() {
        let repo = repo();
        let (ws, chat) = (ws(), chat());
        let p = Participant {
            id: "p1".into(),
            pseudonym: "Swift-Penguin".into(),
        };
        repo.create_participant(&ws, &chat, &p, None, Some("Rob"))
            .unwrap();
        repo.add_alias("p1", "Robert").unwrap();
        repo.add_alias("p1", "Robert").unwrap(); // idempotent
        assert_eq!(
            repo.participant_by_alias(&ws, &chat, "Robert")
                .unwrap()
                .unwrap()
                .id,
            "p1"
        );
        assert_eq!(
            repo.participant_by_alias(&ws, &chat, "Rob")
                .unwrap()
                .unwrap()
                .id,
            "p1"
        );
    }

    #[test]
    fn saving_same_fingerprint_twice_is_idempotent() {
        let repo = repo();
        let ws = ws();
        let m = NormalizedMessage {
            id: MessageId::parse("fp-1").unwrap(),
            platform: Platform::WhatsApp,
            channel_id: chat(),
            author_id: "p1".into(),
            author_name: "Swift-Penguin".into(),
            content: "hello".into(),
            timestamp: 1_700_000_000,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        };
        assert!(repo.save_message(&ws, &m).unwrap()); // new
        assert!(!repo.save_message(&ws, &m).unwrap()); // duplicate id ignored

        // Round-trips through the read path, and is workspace-scoped.
        assert_eq!(repo.get_message(&ws, &m.id).unwrap().as_ref(), Some(&m));
        let other = WorkspaceId::parse("ws-other").unwrap();
        assert!(repo.get_message(&other, &m.id).unwrap().is_none());
    }

    #[test]
    fn stored_attachment_round_trips() {
        let repo = repo();
        let ws = ws();
        let m = NormalizedMessage {
            id: MessageId::parse("fp-2").unwrap(),
            platform: Platform::WhatsApp,
            channel_id: chat(),
            author_id: "p1".into(),
            author_name: "Swift-Penguin".into(),
            content: String::new(),
            timestamp: 1_700_000_000,
            is_system: false,
            reply_to: None,
            attachments: vec![Attachment {
                kind: AttachmentKind::Image,
                filename: Some("photo.jpg".into()),
            }],
        };
        repo.save_message(&ws, &m).unwrap();
        assert_eq!(repo.get_message(&ws, &m.id).unwrap().unwrap(), m);
    }

    #[test]
    fn list_messages_filters_by_channel_and_range_oldest_first() {
        let repo = repo();
        let ws = ws();
        let msg = |id: &str, chan: &str, ts: i64| NormalizedMessage {
            id: MessageId::parse(id).unwrap(),
            platform: Platform::WhatsApp,
            channel_id: ChannelId::parse(chan).unwrap(),
            author_id: "p1".into(),
            author_name: "P".into(),
            content: "hi".into(),
            timestamp: ts,
            is_system: false,
            reply_to: None,
            attachments: vec![],
        };
        repo.save_message(&ws, &msg("a", "c1", 100)).unwrap();
        repo.save_message(&ws, &msg("b", "c1", 300)).unwrap();
        repo.save_message(&ws, &msg("c", "c1", 500)).unwrap(); // out of range
        repo.save_message(&ws, &msg("d", "c2", 200)).unwrap(); // other channel

        let c1 = ChannelId::parse("c1").unwrap();
        let got: Vec<String> = repo
            .list_messages(&ws, &c1, 0, 400)
            .unwrap()
            .into_iter()
            .map(|m| m.id.as_str().to_string())
            .collect();
        assert_eq!(got, vec!["a".to_string(), "b".to_string()]); // c/d excluded, ordered
    }
}

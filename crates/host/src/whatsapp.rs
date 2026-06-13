//! Host-side WhatsApp ingestion (PRD §2.3; ADR-121).
//!
//! Turns the pure parser's [`RawWhatsAppMessage`] intermediate into persisted
//! [`NormalizedMessage`]s. This is the host's half of ADR-121 — the parts that
//! need a key or storage:
//!   * **anonymize** phone numbers to stable pseudonyms (WHA-006, HMAC keyed);
//!   * **resolve identity** to a per-chat participant (phone-hash, then alias,
//!     then "You" → uploader, else create new — WHA-013);
//!   * **fingerprint** each message into its canonical id over the resolved
//!     identity + UTC instant + content (WHA-012/021);
//!   * **dedup** idempotently on that id and persist.
//!
//! Crypto is HMAC/SHA-256 from the same vetted crates as the auth seam. Fuzzy
//! alias matching (ADR-121 §3) is intentionally deferred; resolution here is
//! exact-match + create, which is correct if conservative (it may create two
//! participants for "Rob"/"Robert" until a later merge pass links them).

use anyhow::{anyhow, Result};
use domain::{
    detect_date_order, parse_export, ChannelId, DateOrder, MessageId, NormalizedMessage,
    ParsedExport, Platform, RawWhatsAppMessage, Secret, UserId, WorkspaceId,
};
use hmac::{Hmac, Mac};
use repository::{ImportRecord, Participant, WhatsAppRepository};
use sha2::{Digest, Sha256};
use std::io::Read;

type HmacSha256 = Hmac<Sha256>;

/// Extract the chat transcript from a WhatsApp export upload (WHA-001): a `.zip`
/// (the `_chat.txt`, else the largest `.txt`), or — as a fallback — raw text
/// when the bytes aren't a zip (a directly-uploaded `_chat.txt`).
pub fn extract_whatsapp_text(bytes: &[u8]) -> Result<String> {
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        // Not a zip → assume a raw text transcript.
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    };
    // Collect .txt entries, then prefer `_chat.txt`, else the largest.
    let mut txts: Vec<(usize, u64, bool)> = Vec::new();
    for i in 0..archive.len() {
        let f = archive.by_index(i)?;
        let name = f.name().to_ascii_lowercase();
        if name.ends_with(".txt") {
            txts.push((i, f.size(), name.ends_with("_chat.txt")));
        }
    }
    txts.sort_by_key(|(_, size, is_chat)| (std::cmp::Reverse(*is_chat), std::cmp::Reverse(*size)));
    let idx = txts
        .first()
        .map(|(i, _, _)| *i)
        .ok_or_else(|| anyhow!("no .txt transcript found in the WhatsApp export zip"))?;
    let mut file = archive.by_index(idx)?;
    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Ingest a WhatsApp export upload end-to-end (WHA-001): extract the transcript
/// from `bytes` (zip or raw text), parse it under the declared `timezone`
/// (WHA-020), then anonymize/dedup/persist. The date-component order is **inferred
/// from the data** ([`detect_date_order`]); `order_hint` (a locale default) is
/// used only when the data is ambiguous. Returns the ingest summary and the
/// parsed export. An unknown timezone is an error.
pub fn ingest_whatsapp_zip<R: WhatsAppRepository>(
    repo: &R,
    anon_key: &Secret<Vec<u8>>,
    ctx: &IngestContext,
    bytes: &[u8],
    timezone: &str,
    order_hint: DateOrder,
    now: i64,
) -> Result<(IngestSummary, ParsedExport)> {
    let zone: chrono_tz::Tz = timezone
        .parse()
        .map_err(|_| anyhow!("unknown timezone: {timezone}"))?;
    let text = extract_whatsapp_text(bytes)?;
    let order = detect_date_order(&text).unwrap_or(order_hint);
    let parsed = parse_export(&text, zone, order);
    let summary = WhatsAppIngestor::new(repo, anon_key).ingest(ctx, &parsed.messages)?;

    // Record the import's span so coverage analysis (WHA-016) has data. File-hash
    // dedup (WHA-010) means a re-uploaded identical export records once. Only
    // exports with a real date range anchor a span.
    if let Some((date_start, date_end)) = parsed.date_range {
        let file_hash = hex(&Sha256::digest(bytes));
        let id = format!("imp_{now}_{}", &file_hash[..12.min(file_hash.len())]);
        let outcome = repo.record_import(&ImportRecord {
            id: &id,
            workspace_id: ctx.workspace_id,
            chat_id: ctx.chat_id,
            file_hash: &file_hash,
            uploader: ctx.uploader,
            imported_at: now,
            format: format_tag(parsed.format),
            message_count: parsed.messages.len() as i64,
            date_start,
            date_end,
            group_created_at: parsed.group_created_at(),
        })?;
        // A genuinely new import may have closed a standing ask: mark any scoped
        // invitation now covered as fulfilled, credited to this uploader (WHA-019).
        if matches!(outcome, repository::ImportOutcome::Recorded) {
            crate::coverage::reconcile_invitations(
                repo,
                ctx.workspace_id,
                ctx.chat_id,
                ctx.uploader,
                now,
            )?;
        }
    }
    Ok((summary, parsed))
}

/// Stable lowercase tag for the stored `format` column.
fn format_tag(f: domain::WhatsAppFormat) -> &'static str {
    match f {
        domain::WhatsAppFormat::Ios => "ios",
        domain::WhatsAppFormat::Android => "android",
    }
}

/// Context for one import: where it lands and who uploaded it.
pub struct IngestContext<'a> {
    pub workspace_id: &'a WorkspaceId,
    pub chat_id: &'a ChannelId,
    pub uploader: &'a UserId,
}

/// Outcome of ingesting a batch.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestSummary {
    /// Newly stored (after fingerprint dedup).
    pub stored: u64,
    /// Skipped because the fingerprint was already present (WHA-012).
    pub duplicates: u64,
    /// Participants created during this batch.
    pub new_participants: u64,
}

/// Ingests parsed WhatsApp messages: resolves+anonymizes identities, fingerprints,
/// dedups and persists. The HMAC key (for phone anonymization) is held as a
/// [`Secret`]; `now`/ids are generated host-side.
pub struct WhatsAppIngestor<'a, R> {
    repo: &'a R,
    anon_key: &'a Secret<Vec<u8>>,
}

impl<'a, R: WhatsAppRepository> WhatsAppIngestor<'a, R> {
    pub fn new(repo: &'a R, anon_key: &'a Secret<Vec<u8>>) -> Self {
        Self { repo, anon_key }
    }

    /// Ingest a batch of parsed messages under `ctx`.
    pub fn ingest(
        &self,
        ctx: &IngestContext,
        messages: &[RawWhatsAppMessage],
    ) -> Result<IngestSummary> {
        let mut summary = IngestSummary::default();
        for raw in messages {
            let (author_id, author_name, created) = self.resolve_author(ctx, raw)?;
            if created {
                summary.new_participants += 1;
            }
            let id = self.fingerprint(ctx, raw, &author_id);
            let message = NormalizedMessage {
                id,
                platform: Platform::WhatsApp,
                channel_id: ctx.chat_id.clone(),
                author_id,
                author_name,
                content: raw.content.clone(),
                timestamp: raw.timestamp,
                is_system: raw.is_system,
                reply_to: None,
                attachments: Vec::new(),
            };
            if self.repo.save_message(ctx.workspace_id, &message)? {
                summary.stored += 1;
            } else {
                summary.duplicates += 1;
            }
        }
        Ok(summary)
    }

    /// Resolve a raw sender to a (participant_id, pseudonym), creating one if
    /// unseen. Returns whether a new participant was created.
    fn resolve_author(
        &self,
        ctx: &IngestContext,
        raw: &RawWhatsAppMessage,
    ) -> Result<(String, String, bool)> {
        let Some(sender) = raw.sender.as_deref() else {
            // System line: a stable, non-personal author.
            return Ok(("system".to_string(), "System".to_string(), false));
        };

        // "You" is the uploader's own messages in their export.
        if sender == "You" {
            let pseudonym = format!("You-{}", short_hash(ctx.uploader.as_str().as_bytes()));
            let id = format!("self:{}", ctx.uploader.as_str());
            return Ok((id, pseudonym, false));
        }

        if is_phone_like(sender) {
            let phone_hash = self.phone_hash(sender);
            if let Some(p) =
                self.repo
                    .participant_by_phone(ctx.workspace_id, ctx.chat_id, &phone_hash)?
            {
                return Ok((p.id, p.pseudonym, false));
            }
            let participant = self.new_participant(ctx, phone_hash.as_bytes());
            self.repo.create_participant(
                ctx.workspace_id,
                ctx.chat_id,
                &participant,
                Some(&phone_hash),
                None,
            )?;
            return Ok((participant.id, participant.pseudonym, true));
        }

        // Contact name: match an existing alias, else create a participant whose
        // pseudonym is derived (never the raw name — that's PII), keeping the
        // raw name only as an alias.
        if let Some(p) = self
            .repo
            .participant_by_alias(ctx.workspace_id, ctx.chat_id, sender)?
        {
            return Ok((p.id, p.pseudonym, false));
        }
        let participant = self.new_participant(ctx, sender.as_bytes());
        self.repo.create_participant(
            ctx.workspace_id,
            ctx.chat_id,
            &participant,
            None,
            Some(sender),
        )?;
        Ok((participant.id, participant.pseudonym, true))
    }

    /// Mint a participant. The **pseudonym** is person-stable (derived from the
    /// seed alone, so the same person reads the same label across chats); the
    /// **id** additionally folds in workspace + chat so the global primary key
    /// stays unique when the same person appears in more than one chat.
    fn new_participant(&self, ctx: &IngestContext, seed: &[u8]) -> Participant {
        let pseudonym = short_hash(seed);
        let mut id_seed = Vec::new();
        id_seed.extend_from_slice(ctx.workspace_id.as_str().as_bytes());
        id_seed.push(0);
        id_seed.extend_from_slice(ctx.chat_id.as_str().as_bytes());
        id_seed.push(0);
        id_seed.extend_from_slice(seed);
        Participant {
            id: format!("p:{}", short_hash(&id_seed)),
            pseudonym: format!("Participant-{pseudonym}"),
        }
    }

    /// Keyed (HMAC) hash of a normalized phone number — stable, but a DB leak
    /// can't be reversed to the number (WHA-006).
    fn phone_hash(&self, sender: &str) -> String {
        let normalized: String = sender.chars().filter(|c| c.is_ascii_digit()).collect();
        let mut mac = HmacSha256::new_from_slice(self.anon_key.expose_secret())
            .expect("HMAC accepts any key length");
        mac.update(normalized.as_bytes());
        hex(&mac.finalize().into_bytes())
    }

    /// Canonical message id (WHA-012/021): a fingerprint over the dedup key, so
    /// the same message from two uploaders/zones collapses to one row.
    fn fingerprint(
        &self,
        ctx: &IngestContext,
        raw: &RawWhatsAppMessage,
        author_id: &str,
    ) -> MessageId {
        let mut hasher = Sha256::new();
        let ts_bytes = raw.timestamp.to_be_bytes();
        for part in [
            ctx.workspace_id.as_str().as_bytes(),
            ctx.chat_id.as_str().as_bytes(),
            ts_bytes.as_slice(),
            author_id.as_bytes(),
            raw.content.as_bytes(),
        ] {
            hasher.update((part.len() as u64).to_be_bytes()); // length-prefixed
            hasher.update(part);
        }
        MessageId::parse(format!("wa_{}", hex(&hasher.finalize())))
            .expect("hex fingerprint is a valid id")
    }
}

/// A sender string that looks like a phone number (WhatsApp shows these for
/// contacts not in the exporter's address book), e.g. "+1 555 123 4567".
fn is_phone_like(sender: &str) -> bool {
    let s = sender.trim();
    s.starts_with('+')
        && s.chars().filter(|c| c.is_ascii_digit()).count() >= 7
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '+' || c == ' ' || c == '-' || c == '(' || c == ')')
}

/// First 12 hex chars of a SHA-256 — a compact, stable, non-reversible token.
fn short_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())[..12].to_string()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{parse_export, DateOrder};
    use repository::SqliteRepository;

    fn key() -> Secret<Vec<u8>> {
        Secret::new(b"anon-key".to_vec())
    }

    fn ctx<'a>(ws: &'a WorkspaceId, chat: &'a ChannelId, up: &'a UserId) -> IngestContext<'a> {
        IngestContext {
            workspace_id: ws,
            chat_id: chat,
            uploader: up,
        }
    }

    fn ingest_text(repo: &SqliteRepository, key: &Secret<Vec<u8>>, text: &str) -> IngestSummary {
        let parsed = parse_export(text, chrono_tz::Europe::London, DateOrder::DayMonthYear);
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let chat = ChannelId::parse("chat-1").unwrap();
        let up = UserId::parse("uploader-1").unwrap();
        WhatsAppIngestor::new(repo, key)
            .ingest(&ctx(&ws, &chat, &up), &parsed.messages)
            .unwrap()
    }

    #[test]
    fn ingests_and_is_idempotent_on_reimport() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let text = "[01/01/2026, 09:00:00] Alice: Good morning\n\
                    [01/01/2026, 09:01:00] Bob: Morning Alice";
        let first = ingest_text(&repo, &key, text);
        assert_eq!(first.stored, 2);
        assert_eq!(first.duplicates, 0);
        assert_eq!(first.new_participants, 2);

        // Re-importing the same content stores nothing new (WHA-012).
        let second = ingest_text(&repo, &key, text);
        assert_eq!(second.stored, 0);
        assert_eq!(second.duplicates, 2);
    }

    #[test]
    fn same_person_across_messages_resolves_to_one_participant() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let s = ingest_text(
            &repo,
            &key,
            "[01/01/2026, 09:00:00] Alice: one\n\
             [01/01/2026, 09:05:00] Alice: two\n\
             [01/01/2026, 09:06:00] Alice: three",
        );
        // Three messages, but Alice is created once.
        assert_eq!(s.stored, 3);
        assert_eq!(s.new_participants, 1);
    }

    #[test]
    fn phone_senders_are_anonymized_never_stored_raw() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let chat = ChannelId::parse("chat-1").unwrap();
        let up = UserId::parse("uploader-1").unwrap();
        let parsed = parse_export(
            "[01/01/2026, 09:00:00] +1 555 123 4567: hi there",
            chrono_tz::Europe::London,
            DateOrder::DayMonthYear,
        );
        let ingestor = WhatsAppIngestor::new(&repo, &key);
        ingestor
            .ingest(&ctx(&ws, &chat, &up), &parsed.messages)
            .unwrap();
        // The resolved author must be a pseudonym, never the raw phone number.
        let (author_id, author_name, _) = ingestor
            .resolve_author(&ctx(&ws, &chat, &up), &parsed.messages[0])
            .unwrap();
        assert!(!author_name.contains("555"));
        assert!(author_id.starts_with("p:"));
    }

    #[test]
    fn is_phone_like_detects_numbers_not_names() {
        assert!(is_phone_like("+1 555 123 4567"));
        assert!(is_phone_like("+44 7700 900123"));
        assert!(!is_phone_like("Alice"));
        assert!(!is_phone_like("Rob Smith"));
    }

    #[test]
    fn cross_uploader_same_message_dedups_via_fingerprint() {
        // Two uploads of the same instant+author+content (e.g. two members'
        // exports) collapse to one stored message.
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let a = ingest_text(&repo, &key, "[01/01/2026, 09:00:00] Alice: shared message");
        assert_eq!(a.stored, 1);
        let b = ingest_text(&repo, &key, "[01/01/2026, 09:00:00] Alice: shared message");
        assert_eq!(b.stored, 0);
        assert_eq!(b.duplicates, 1);
    }

    #[test]
    fn same_person_in_two_chats_does_not_collide_on_id() {
        // Regression: the participant id must be unique per (workspace, chat),
        // so importing the same person into a second chat doesn't hit the PK.
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let up = UserId::parse("u1").unwrap();
        let phone_text = "[01/01/2026, 09:00:00] +1 (902) 702-3100: hello";
        let name_text = "[01/01/2026, 09:00:00] John O'Hare: hi";
        for text in [phone_text, name_text] {
            let parsed = parse_export(text, chrono_tz::UTC, DateOrder::DayMonthYear);
            let a = ChannelId::parse("chat-a").unwrap();
            let b = ChannelId::parse("chat-b").unwrap();
            WhatsAppIngestor::new(&repo, &key)
                .ingest(&ctx(&ws, &a, &up), &parsed.messages)
                .unwrap();
            // Same sender, different chat — previously a UNIQUE(id) violation.
            let s = WhatsAppIngestor::new(&repo, &key)
                .ingest(&ctx(&ws, &b, &up), &parsed.messages)
                .unwrap();
            assert_eq!(s.stored, 1);
            assert_eq!(s.new_participants, 1);
        }
    }

    /// Build an in-memory zip from (name, content) entries.
    fn make_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write;
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            for (name, content) in entries {
                w.start_file(*name, SimpleFileOptions::default()).unwrap();
                w.write_all(content.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_prefers_chat_txt_then_largest() {
        let zip = make_zip(&[
            ("media/note.txt", "irrelevant"),
            ("WhatsApp Chat/_chat.txt", "the real transcript"),
        ]);
        assert_eq!(extract_whatsapp_text(&zip).unwrap(), "the real transcript");
        // No zip → treated as raw text.
        assert_eq!(
            extract_whatsapp_text(b"raw _chat text").unwrap(),
            "raw _chat text"
        );
    }

    #[test]
    fn ingests_a_zip_export_end_to_end() {
        let repo = SqliteRepository::in_memory().unwrap();
        let key = key();
        let zip = make_zip(&[(
            "WhatsApp Chat with Team/_chat.txt",
            "[01/01/2026, 09:00:00] Alice: hello team\n[01/01/2026, 09:01:00] Bob: morning",
        )]);
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let chat = ChannelId::parse("c1").unwrap();
        let up = UserId::parse("u1").unwrap();
        let (summary, parsed) = ingest_whatsapp_zip(
            &repo,
            &key,
            &ctx(&ws, &chat, &up),
            &zip,
            "UTC",
            DateOrder::DayMonthYear,
            1_700_000_000,
        )
        .unwrap();
        assert_eq!(summary.stored, 2);
        assert_eq!(parsed.messages.len(), 2);
        assert!(parsed.date_range.is_some());
        // The import span was recorded (WHA-009) so coverage has data.
        let imports = repo.list_imports(&ws, &chat).unwrap();
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].message_count, 2);
    }

    #[test]
    fn ingest_zip_rejects_unknown_timezone() {
        let repo = SqliteRepository::in_memory().unwrap();
        let ws = WorkspaceId::parse("ws-1").unwrap();
        let chat = ChannelId::parse("c1").unwrap();
        let up = UserId::parse("u1").unwrap();
        let err = ingest_whatsapp_zip(
            &repo,
            &key(),
            &ctx(&ws, &chat, &up),
            b"[01/01/2026, 09:00:00] Alice: hi",
            "Mars/Olympus",
            DateOrder::DayMonthYear,
            1_700_000_000,
        );
        assert!(err.is_err());
    }
}

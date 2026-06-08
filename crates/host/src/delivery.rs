//! Delivery dispatch (PRD §4) — host orchestration over the domain gating
//! policy + the always-on dashboard store.
//!
//! Every summary is **always** persisted to the structured store (the dashboard
//! destination, PRD §4 item 3). Additional destinations are gated by the pure
//! [`domain::resolve_delivery`] policy (DEL-010/011, DEN-*) and, if allowed,
//! rendered and handed to a registered [`Deliverer`]. Concrete deliverers
//! (Discord/Slack/email/webhook) do real network I/O and land with the Phase 5
//! runtime; this defines the seam and a fake for testing the dispatch logic.

use domain::summarize::{render, SummaryFormat};
use domain::{
    resolve_delivery, DeliveryCapabilities, DeliveryDecision, DeliveryReject, Destination,
    DestinationKind, WorkspaceId,
};
use repository::{DestinationRepository, StructuredSummaryRepository, SummaryRecord};

/// A sender for one non-dashboard destination kind. Sync for now (matches the
/// codebase); real network impls arrive with the async runtime.
pub trait Deliverer {
    fn kind(&self) -> DestinationKind;
    /// Send the already-rendered summary to `dest`. `Err` is a transport/format
    /// failure message.
    fn deliver(&self, dest: &Destination, rendered: &str) -> Result<(), String>;
}

/// Per-destination result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Persisted to the always-on dashboard store.
    Stored,
    /// Sent to an external destination.
    Delivered,
    /// Refused by the gating policy (DEL-010/011).
    Rejected(DeliveryReject),
    /// Allowed but the send failed (no deliverer registered, or transport error).
    Failed(String),
}

/// What happened across all destinations for one summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReport {
    pub results: Vec<(DestinationKind, DeliveryOutcome)>,
}

impl DeliveryReport {
    pub fn delivered_count(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, o)| matches!(o, DeliveryOutcome::Delivered | DeliveryOutcome::Stored))
            .count()
    }
}

/// Stores every summary to the dashboard and fans out to allowed destinations.
/// Deliverers are *borrowed* so a caller (e.g. the scheduler) can build them
/// once and reuse the set across many summaries.
pub struct DeliveryService<'a, R> {
    store: &'a R,
    deliverers: &'a [Box<dyn Deliverer + 'a>],
}

impl<'a, R: StructuredSummaryRepository> DeliveryService<'a, R> {
    pub fn new(store: &'a R) -> Self {
        Self {
            store,
            deliverers: &[],
        }
    }

    /// Register the deliverer set used for external (non-dashboard) destinations.
    pub fn with_deliverers(mut self, deliverers: &'a [Box<dyn Deliverer + 'a>]) -> Self {
        self.deliverers = deliverers;
        self
    }

    /// Persist to the dashboard (always-on) and deliver to each requested extra
    /// destination, gated by the workspace capabilities.
    pub fn deliver(
        &self,
        workspace: &WorkspaceId,
        record: &SummaryRecord,
        destinations: &[Destination],
        caps: &DeliveryCapabilities,
        format: SummaryFormat,
    ) -> anyhow::Result<DeliveryReport> {
        let mut results = Vec::new();

        // Always-on dashboard store (PRD §4 item 3).
        self.store.save_record(workspace, record)?;
        results.push((DestinationKind::Dashboard, DeliveryOutcome::Stored));

        let rendered = render(&record.summary, format);
        for dest in destinations {
            if dest.kind == DestinationKind::Dashboard {
                continue; // already stored, unconditionally
            }
            let outcome = match resolve_delivery(dest, caps) {
                DeliveryDecision::Rejected(reason) => DeliveryOutcome::Rejected(reason),
                DeliveryDecision::Allowed => self.dispatch(dest, &rendered),
            };
            results.push((dest.kind, outcome));
        }
        Ok(DeliveryReport { results })
    }

    fn dispatch(&self, dest: &Destination, rendered: &str) -> DeliveryOutcome {
        match self.deliverers.iter().find(|d| d.kind() == dest.kind) {
            Some(d) => match d.deliver(dest, rendered) {
                Ok(()) => DeliveryOutcome::Delivered,
                Err(e) => DeliveryOutcome::Failed(e),
            },
            None => DeliveryOutcome::Failed(format!("no deliverer for {:?}", dest.kind)),
        }
    }
}

/// String form of a [`DestinationKind`] as stored in the repository.
pub fn kind_str(kind: DestinationKind) -> &'static str {
    match kind {
        DestinationKind::Dashboard => "dashboard",
        DestinationKind::PlatformChannel => "platform_channel",
        DestinationKind::PlatformDm => "platform_dm",
        DestinationKind::Email => "email",
        DestinationKind::Webhook => "webhook",
    }
}

/// Parse a stored `kind` string back to a [`DestinationKind`] (`None` if unknown
/// — a forward-compatible row this build doesn't understand).
pub fn parse_kind(kind: &str) -> Option<DestinationKind> {
    Some(match kind {
        "dashboard" => DestinationKind::Dashboard,
        "platform_channel" => DestinationKind::PlatformChannel,
        "platform_dm" => DestinationKind::PlatformDm,
        "email" => DestinationKind::Email,
        "webhook" => DestinationKind::Webhook,
        _ => return None,
    })
}

/// Build the [`Destination`] list + [`DeliveryCapabilities`] for a workspace from
/// its stored destinations (DSH-010/011). Decrypts each address with the operator
/// master key; a row that can't be decrypted (no key, or tampered) is skipped and
/// not marked configured, so [`resolve_delivery`] rejects it rather than sending
/// to a bad address. Only **enabled** destinations are returned for sending; a
/// kind is `enabled`/`configured` in the caps when it has at least one such row.
pub fn load_workspace_delivery(
    repo: &impl DestinationRepository,
    workspace: &WorkspaceId,
    master: Option<&[u8; 32]>,
) -> anyhow::Result<(Vec<Destination>, DeliveryCapabilities)> {
    let mut destinations = Vec::new();
    let mut caps = DeliveryCapabilities::default();
    for row in repo.list_destinations(workspace)? {
        if !row.enabled {
            continue;
        }
        let Some(kind) = parse_kind(&row.kind) else {
            continue;
        };
        // Decrypt the address; skip rows we can't read (missing key / bad cipher).
        let address = match (&row.address_enc, master) {
            (Some(enc), Some(key)) => match crate::decrypt_secret(key, enc) {
                Ok(plain) => Some(plain),
                Err(_) => continue,
            },
            _ => continue,
        };
        if !caps.enabled.contains(&kind) {
            caps.enabled.push(kind);
        }
        if !caps.configured.contains(&kind) {
            caps.configured.push(kind);
        }
        destinations.push(Destination {
            kind,
            platform: None,
            address,
        });
    }
    Ok((destinations, caps))
}

/// Concrete webhook deliverer (DSH-010): POST the rendered summary as JSON to a
/// configured URL. Covers generic webhooks and incoming-webhook URLs for Slack
/// (`{"text": ...}`) / Discord (`{"content": ...}`) — we send all three keys so a
/// single payload satisfies the common receivers. Network I/O, so feature-gated
/// like the HTTP LLM client; the default build stays offline.
#[cfg(feature = "http-llm")]
pub struct WebhookDeliverer {
    timeout_secs: u64,
}

#[cfg(feature = "http-llm")]
impl Default for WebhookDeliverer {
    fn default() -> Self {
        Self { timeout_secs: 15 }
    }
}

#[cfg(feature = "http-llm")]
impl Deliverer for WebhookDeliverer {
    fn kind(&self) -> DestinationKind {
        DestinationKind::Webhook
    }

    fn deliver(&self, dest: &Destination, rendered: &str) -> Result<(), String> {
        let url = dest
            .address
            .as_deref()
            .filter(|u| !u.is_empty())
            .ok_or_else(|| "webhook destination has no url".to_string())?;
        // `text`/`content` satisfy Slack/Discord incoming webhooks; `summary`
        // is the generic field. The body is the already-rendered markdown.
        let body = serde_json::json!({
            "text": rendered,
            "content": rendered,
            "summary": rendered,
        });
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        match agent.post(url).send_json(body) {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(format!("webhook returned http {code}")),
            Err(ureq::Error::Transport(t)) => Err(format!("webhook transport error: {t}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::summarize::{ExtractedSummary, ResolvedCitation};
    use domain::{MessageId, Platform};
    use repository::SqliteRepository;
    use std::cell::RefCell;

    fn ws() -> WorkspaceId {
        WorkspaceId::parse("ws-1").unwrap()
    }

    fn record() -> SummaryRecord {
        SummaryRecord {
            id: "sum_1".into(),
            channel_id: None,
            model: "sonnet".into(),
            cost_micros: 1_000,
            degraded: false,
            created_at: 1,
            pinned: false,
            archived: false,
            tags: vec![],
            summary: ExtractedSummary {
                text: "We shipped.".into(),
                key_points: vec!["Launched".into()],
                technical_terms: vec![],
                participants: vec!["Alice".into()],
                action_items: vec![],
                citations: vec![ResolvedCitation {
                    message_id: MessageId::parse("m0").unwrap(),
                    quote: None,
                }],
            },
        }
    }

    /// Records what it was asked to deliver.
    struct SpyDeliverer {
        kind: DestinationKind,
        sent: RefCell<Vec<String>>,
    }
    impl Deliverer for SpyDeliverer {
        fn kind(&self) -> DestinationKind {
            self.kind
        }
        fn deliver(&self, _dest: &Destination, rendered: &str) -> Result<(), String> {
            self.sent.borrow_mut().push(rendered.to_string());
            Ok(())
        }
    }

    fn chan(platform: Platform, addr: &str) -> Destination {
        Destination {
            kind: DestinationKind::PlatformChannel,
            platform: Some(platform),
            address: Some(addr.to_string()),
        }
    }

    #[test]
    fn always_stores_to_dashboard_even_with_no_destinations() {
        let store = SqliteRepository::in_memory().unwrap();
        let svc = DeliveryService::new(&store);
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[],
                &DeliveryCapabilities::default(),
                SummaryFormat::Markdown,
            )
            .unwrap();
        assert_eq!(
            report.results,
            vec![(DestinationKind::Dashboard, DeliveryOutcome::Stored)]
        );
        // And it's actually persisted.
        assert!(store.get_record(&ws(), "sum_1").unwrap().is_some());
    }

    #[test]
    fn allowed_platform_destination_is_delivered_rendered() {
        let store = SqliteRepository::in_memory().unwrap();
        let deliverers: Vec<Box<dyn Deliverer>> = vec![Box::new(SpyDeliverer {
            kind: DestinationKind::PlatformChannel,
            sent: RefCell::new(vec![]),
        })];
        let svc = DeliveryService::new(&store).with_deliverers(&deliverers);
        let caps = DeliveryCapabilities {
            connected_platforms: vec![Platform::Discord],
            enabled: vec![DestinationKind::PlatformChannel],
            configured: vec![],
        };
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[chan(Platform::Discord, "c1")],
                &caps,
                SummaryFormat::Markdown,
            )
            .unwrap();
        assert!(report
            .results
            .contains(&(DestinationKind::PlatformChannel, DeliveryOutcome::Delivered)));
        assert_eq!(report.delivered_count(), 2); // dashboard + channel
    }

    #[test]
    fn unconnected_platform_destination_is_rejected() {
        let store = SqliteRepository::in_memory().unwrap();
        let svc = DeliveryService::new(&store);
        let caps = DeliveryCapabilities {
            enabled: vec![DestinationKind::PlatformChannel],
            ..DeliveryCapabilities::default()
        };
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[chan(Platform::Discord, "c1")],
                &caps,
                SummaryFormat::Markdown,
            )
            .unwrap();
        assert!(report.results.contains(&(
            DestinationKind::PlatformChannel,
            DeliveryOutcome::Rejected(DeliveryReject::PlatformNotConnected)
        )));
    }

    #[test]
    fn allowed_but_unregistered_destination_fails() {
        let store = SqliteRepository::in_memory().unwrap();
        let svc = DeliveryService::new(&store); // no deliverers registered
        let caps = DeliveryCapabilities {
            connected_platforms: vec![Platform::Discord],
            enabled: vec![DestinationKind::PlatformChannel],
            ..DeliveryCapabilities::default()
        };
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[chan(Platform::Discord, "c1")],
                &caps,
                SummaryFormat::Markdown,
            )
            .unwrap();
        let (_, outcome) = report
            .results
            .iter()
            .find(|(k, _)| *k == DestinationKind::PlatformChannel)
            .unwrap();
        assert!(matches!(outcome, DeliveryOutcome::Failed(_)));
    }

    #[test]
    fn kind_string_round_trips() {
        for k in [
            DestinationKind::Dashboard,
            DestinationKind::PlatformChannel,
            DestinationKind::PlatformDm,
            DestinationKind::Email,
            DestinationKind::Webhook,
        ] {
            assert_eq!(parse_kind(kind_str(k)), Some(k));
        }
        assert_eq!(parse_kind("from-the-future"), None);
    }

    #[test]
    fn load_workspace_delivery_decrypts_enabled_and_marks_caps() {
        use repository::{DestinationRepository, StoredDestination};
        let master = [7u8; 32];
        let store = SqliteRepository::in_memory().unwrap();
        let enc = crate::encrypt_secret(&master, "https://hooks.example/abc").unwrap();
        store
            .upsert_destination(
                &ws(),
                &StoredDestination {
                    id: "d1".into(),
                    kind: "webhook".into(),
                    address_enc: Some(enc),
                    enabled: true,
                    created_at: 1,
                },
            )
            .unwrap();
        // A disabled row is ignored entirely.
        store
            .upsert_destination(
                &ws(),
                &StoredDestination {
                    id: "d2".into(),
                    kind: "webhook".into(),
                    address_enc: Some(crate::encrypt_secret(&master, "https://off").unwrap()),
                    enabled: false,
                    created_at: 2,
                },
            )
            .unwrap();

        let (dests, caps) = load_workspace_delivery(&store, &ws(), Some(&master)).unwrap();
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].kind, DestinationKind::Webhook);
        assert_eq!(
            dests[0].address.as_deref(),
            Some("https://hooks.example/abc")
        );
        assert!(caps.enabled.contains(&DestinationKind::Webhook));
        assert!(caps.configured.contains(&DestinationKind::Webhook));

        // The decrypted destination passes the gate.
        assert_eq!(
            resolve_delivery(&dests[0], &caps),
            DeliveryDecision::Allowed
        );
    }

    #[test]
    fn load_workspace_delivery_skips_undecryptable_rows() {
        use repository::{DestinationRepository, StoredDestination};
        let store = SqliteRepository::in_memory().unwrap();
        store
            .upsert_destination(
                &ws(),
                &StoredDestination {
                    id: "d1".into(),
                    kind: "webhook".into(),
                    address_enc: Some("not-decryptable".into()),
                    enabled: true,
                    created_at: 1,
                },
            )
            .unwrap();
        // No master key configured at all → nothing usable.
        let (dests, caps) = load_workspace_delivery(&store, &ws(), None).unwrap();
        assert!(dests.is_empty());
        assert!(caps.enabled.is_empty());
    }
}

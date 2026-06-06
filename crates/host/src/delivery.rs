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
use repository::{StructuredSummaryRepository, SummaryRecord};

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
pub struct DeliveryService<'a, R> {
    store: &'a R,
    deliverers: Vec<Box<dyn Deliverer + 'a>>,
}

impl<'a, R: StructuredSummaryRepository> DeliveryService<'a, R> {
    pub fn new(store: &'a R) -> Self {
        Self {
            store,
            deliverers: Vec::new(),
        }
    }

    pub fn with_deliverer(mut self, deliverer: Box<dyn Deliverer + 'a>) -> Self {
        self.deliverers.push(deliverer);
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
        let spy = Box::new(SpyDeliverer {
            kind: DestinationKind::PlatformChannel,
            sent: RefCell::new(vec![]),
        });
        let svc = DeliveryService::new(&store).with_deliverer(spy);
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
}

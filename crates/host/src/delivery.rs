//! Delivery dispatch (PRD §4; ADR-126 plugin sinks) — host orchestration over
//! the domain gating policy + the always-on dashboard store.
//!
//! Every summary is **always** persisted to the structured store (the dashboard
//! destination, PRD §4 item 3). Additional destinations are gated by the pure
//! [`domain::resolve_delivery`] policy (DEL-010/011, DEN-*) and, if allowed,
//! rendered and handed to a registered [`Deliverer`].
//!
//! Sinks are **plugins** (ADR-126): each has a string `id` ("webhook",
//! "confluence", …), a config schema, and a `Deliverer`. They're compiled in
//! behind cargo features (network-free by default) and enabled + configured per
//! workspace. A destination's config is an encrypted JSON blob (one field for a
//! webhook URL, several for Confluence); the repository never sees plaintext.

use domain::summarize::{render, SummaryFormat};
use domain::{
    resolve_delivery, DeliveryCapabilities, DeliveryClass, DeliveryDecision, DeliveryReject,
    Destination, WorkspaceId,
};
use repository::{DestinationRepository, StructuredSummaryRepository, SummaryRecord};
use serde_json::Value;

/// A sink plugin: sends an already-rendered summary to one destination kind,
/// given that destination's decrypted JSON config. Sync for now (matches the
/// codebase); object-safe so plugins compose in a registry.
pub trait Deliverer {
    /// The destination kind id this plugin handles ("webhook", "confluence", …).
    fn id(&self) -> &str;
    /// Send `rendered` using `config` (the decrypted per-destination JSON).
    /// `Err` is a transport/config failure message.
    fn deliver(&self, config: &Value, rendered: &str) -> Result<(), String>;
}

/// How a config field's value may be surfaced back to the client (the value is
/// always stored encrypted; this only controls the non-secret display hint).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldHint {
    /// Show the value as-is (e.g. a base URL, a space key).
    Full,
    /// Show only `scheme://host` of a URL (e.g. a webhook URL with a secret path).
    Host,
    /// Never echo (e.g. an API token).
    None,
}

/// One field of a sink plugin's config schema.
#[derive(Debug, Clone, Copy)]
pub struct FieldSpec {
    pub name: &'static str,
    pub label: &'static str,
    /// Render as a password input and never echo the value back.
    pub secret: bool,
    pub required: bool,
    pub hint: FieldHint,
}

/// A sink plugin's descriptor — drives API validation and the dashboard form.
#[derive(Debug, Clone)]
pub struct SinkDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub fields: &'static [FieldSpec],
}

/// Descriptors for every sink plugin compiled into this build (ADR-126). The
/// dashboard renders a config form from these; the API validates against them.
// Conditional (cfg-gated) pushes, so a vec! literal won't do.
#[allow(clippy::vec_init_then_push)]
pub fn sink_descriptors() -> Vec<SinkDescriptor> {
    #[allow(unused_mut)]
    let mut out: Vec<SinkDescriptor> = Vec::new();
    #[cfg(feature = "http-llm")]
    out.push(WEBHOOK_DESCRIPTOR);
    #[cfg(feature = "confluence")]
    out.push(CONFLUENCE_DESCRIPTOR);
    #[cfg(feature = "email")]
    out.push(EMAIL_DESCRIPTOR);
    out
}

/// The deliverer set for every sink plugin compiled into this build.
#[allow(clippy::vec_init_then_push)]
pub fn build_deliverers() -> Vec<Box<dyn Deliverer>> {
    #[allow(unused_mut)]
    let mut out: Vec<Box<dyn Deliverer>> = Vec::new();
    #[cfg(feature = "http-llm")]
    out.push(Box::new(WebhookDeliverer::default()));
    #[cfg(feature = "confluence")]
    out.push(Box::new(ConfluenceDeliverer::default()));
    #[cfg(feature = "email")]
    out.push(Box::new(EmailDeliverer));
    out
}

/// A gating destination paired with its decrypted config, ready to dispatch.
pub struct ConfiguredDestination {
    pub dest: Destination,
    pub config: Value,
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

/// What happened across all destinations for one summary. Keyed by the open kind
/// string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryReport {
    pub results: Vec<(String, DeliveryOutcome)>,
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

    /// Register the sink-plugin set used for external (non-dashboard) destinations.
    pub fn with_deliverers(mut self, deliverers: &'a [Box<dyn Deliverer + 'a>]) -> Self {
        self.deliverers = deliverers;
        self
    }

    /// Persist to the dashboard (always-on) and deliver to each configured extra
    /// destination, gated by the workspace capabilities.
    pub fn deliver(
        &self,
        workspace: &WorkspaceId,
        record: &SummaryRecord,
        destinations: &[ConfiguredDestination],
        caps: &DeliveryCapabilities,
        format: SummaryFormat,
    ) -> anyhow::Result<DeliveryReport> {
        let mut results = Vec::new();

        // Always-on dashboard store (PRD §4 item 3).
        self.store.save_record(workspace, record)?;
        results.push(("dashboard".to_string(), DeliveryOutcome::Stored));

        let rendered = render(&record.summary, format);
        for cd in destinations {
            if cd.dest.class == DeliveryClass::Dashboard {
                continue; // already stored, unconditionally
            }
            let outcome = match resolve_delivery(&cd.dest, caps) {
                DeliveryDecision::Rejected(reason) => DeliveryOutcome::Rejected(reason),
                DeliveryDecision::Allowed => self.dispatch(&cd.dest.kind, &cd.config, &rendered),
            };
            results.push((cd.dest.kind.clone(), outcome));
        }
        Ok(DeliveryReport { results })
    }

    fn dispatch(&self, kind: &str, config: &Value, rendered: &str) -> DeliveryOutcome {
        match self.deliverers.iter().find(|d| d.id() == kind) {
            Some(d) => match d.deliver(config, rendered) {
                Ok(()) => DeliveryOutcome::Delivered,
                Err(e) => DeliveryOutcome::Failed(e),
            },
            None => DeliveryOutcome::Failed(format!("no deliverer for {kind}")),
        }
    }
}

/// Decrypt a stored destination's config blob. New rows hold a JSON object;
/// legacy webhook rows held a bare URL string — wrap those as `{"url": …}` so
/// downstream always sees an object.
fn decode_config(master: Option<&[u8; 32]>, enc: &str) -> Option<Value> {
    let key = master?;
    let plain = crate::decrypt_secret(key, enc).ok()?;
    match serde_json::from_str::<Value>(&plain) {
        Ok(v) if v.is_object() => Some(v),
        _ => Some(serde_json::json!({ "url": plain })),
    }
}

/// Build the configured-destination list + [`DeliveryCapabilities`] for a
/// workspace from its stored destinations (DSH-010/011, ADR-126). Decrypts each
/// config with the operator master key; a row that can't be decrypted (no key,
/// or tampered) is skipped and not marked configured, so [`resolve_delivery`]
/// rejects it rather than sending to a bad address. All stored sinks are
/// `Service`-class; only **enabled** rows are returned.
pub fn load_workspace_delivery(
    repo: &impl DestinationRepository,
    workspace: &WorkspaceId,
    master: Option<&[u8; 32]>,
) -> anyhow::Result<(Vec<ConfiguredDestination>, DeliveryCapabilities)> {
    let mut destinations = Vec::new();
    let mut caps = DeliveryCapabilities::default();
    for row in repo.list_destinations(workspace)? {
        if !row.enabled {
            continue;
        }
        let Some(enc) = &row.address_enc else {
            continue;
        };
        let Some(config) = decode_config(master, enc) else {
            continue;
        };
        if !caps.enabled.contains(&row.kind) {
            caps.enabled.push(row.kind.clone());
        }
        if !caps.configured.contains(&row.kind) {
            caps.configured.push(row.kind.clone());
        }
        destinations.push(ConfiguredDestination {
            dest: Destination::service(row.kind),
            config,
        });
    }
    Ok((destinations, caps))
}

// ---- webhook sink plugin (ADR-126; DSH-010) --------------------------------

/// Webhook config schema: a single URL (secret path, host-only hint).
#[cfg(feature = "http-llm")]
const WEBHOOK_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "webhook",
    display_name: "Webhook",
    fields: &[FieldSpec {
        name: "url",
        label: "Webhook URL",
        secret: true,
        required: true,
        hint: FieldHint::Host,
    }],
};

/// POST the rendered summary as JSON to a configured URL. Covers generic
/// webhooks and incoming-webhook URLs for Slack (`{"text":…}`) / Discord
/// (`{"content":…}`) — all three keys are sent so one payload satisfies the
/// common receivers.
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
    fn id(&self) -> &str {
        "webhook"
    }

    fn deliver(&self, config: &Value, rendered: &str) -> Result<(), String> {
        let url = config
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| "webhook destination has no url".to_string())?;
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

// ---- Confluence sink plugin (ADR-126; legacy ADR-099) ----------------------

/// Confluence Cloud config: base URL, space key, account email + API token.
#[cfg(feature = "confluence")]
const CONFLUENCE_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "confluence",
    display_name: "Confluence",
    fields: &[
        FieldSpec {
            name: "base_url",
            label: "Base URL (e.g. https://acme.atlassian.net)",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "space_key",
            label: "Space key",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "email",
            label: "Account email",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "api_token",
            label: "API token",
            secret: true,
            required: true,
            hint: FieldHint::None,
        },
    ],
};

/// Publish each summary as a new Confluence page (DEL-006). Cloud REST v1:
/// `POST {base_url}/wiki/rest/api/content` with Basic auth (`email:api_token`).
/// The rendered markdown is wrapped as minimal storage-format XHTML — rich
/// markdown→storage conversion is a later refinement.
#[cfg(feature = "confluence")]
pub struct ConfluenceDeliverer {
    timeout_secs: u64,
}

#[cfg(feature = "confluence")]
impl Default for ConfluenceDeliverer {
    fn default() -> Self {
        Self { timeout_secs: 20 }
    }
}

#[cfg(feature = "confluence")]
impl Deliverer for ConfluenceDeliverer {
    fn id(&self) -> &str {
        "confluence"
    }

    fn deliver(&self, config: &Value, rendered: &str) -> Result<(), String> {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;

        let field = |k: &str| config.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let base_url = field("base_url").trim_end_matches('/');
        let space_key = field("space_key");
        let email = field("email");
        let token = field("api_token");
        if base_url.is_empty() || space_key.is_empty() || email.is_empty() || token.is_empty() {
            return Err("confluence config is incomplete".to_string());
        }

        let title = confluence_title(rendered);
        let body = serde_json::json!({
            "type": "page",
            "title": title,
            "space": { "key": space_key },
            "body": {
                "storage": { "value": to_storage_html(rendered), "representation": "storage" }
            }
        });
        let auth = format!("Basic {}", STANDARD.encode(format!("{email}:{token}")));
        let endpoint = format!("{base_url}/wiki/rest/api/content");
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        match agent
            .post(&endpoint)
            .set("Authorization", &auth)
            .set("Content-Type", "application/json")
            .send_json(body)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(format!("confluence returned http {code}")),
            Err(ureq::Error::Transport(t)) => Err(format!("confluence transport error: {t}")),
        }
    }
}

/// A page title from the summary's first line, made unique (Confluence titles
/// are unique per space) with a clock suffix.
#[cfg(feature = "confluence")]
fn confluence_title(rendered: &str) -> String {
    let first = rendered
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Summary")
        .trim_start_matches('#')
        .trim();
    let snippet: String = first.chars().take(80).collect();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("SummaryBot: {snippet} ({ts})")
}

/// Minimal markdown→Confluence storage XHTML: escape, newlines→`<br/>`, wrap.
#[cfg(feature = "confluence")]
fn to_storage_html(rendered: &str) -> String {
    let escaped = rendered
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\n', "<br/>");
    format!("<p>{escaped}</p>")
}

// ---- email (SMTP) sink plugin (ADR-126; legacy ADR-030) --------------------

/// SMTP config: server + port, login credentials, and from/to addresses.
#[cfg(feature = "email")]
const EMAIL_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "email",
    display_name: "Email (SMTP)",
    fields: &[
        FieldSpec {
            name: "smtp_host",
            label: "SMTP host (e.g. smtp.gmail.com)",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "smtp_port",
            label: "SMTP port (587 STARTTLS / 465 TLS)",
            secret: false,
            required: false,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "username",
            label: "Username",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "password",
            label: "Password / app password",
            secret: true,
            required: true,
            hint: FieldHint::None,
        },
        FieldSpec {
            name: "from",
            label: "From address",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
        FieldSpec {
            name: "to",
            label: "To address",
            secret: false,
            required: true,
            hint: FieldHint::Full,
        },
    ],
};

/// Send each summary as a plain-text email over SMTP (DEL-004). Port 465 uses
/// implicit TLS; anything else (default 587) uses STARTTLS. Single recipient for
/// now — multiple recipients are a later refinement.
#[cfg(feature = "email")]
pub struct EmailDeliverer;

#[cfg(feature = "email")]
impl Deliverer for EmailDeliverer {
    fn id(&self) -> &str {
        "email"
    }

    fn deliver(&self, config: &Value, rendered: &str) -> Result<(), String> {
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{Message, SmtpTransport, Transport};

        let field = |k: &str| config.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let host = field("smtp_host");
        let username = field("username");
        let password = field("password");
        let from = field("from");
        let to = field("to");
        if host.is_empty()
            || username.is_empty()
            || password.is_empty()
            || from.is_empty()
            || to.is_empty()
        {
            return Err("email config is incomplete".to_string());
        }
        let port: u16 = match field("smtp_port") {
            "" => 587,
            p => p.parse().map_err(|_| format!("invalid smtp_port: {p}"))?,
        };

        let subject = email_subject(rendered);
        let message = Message::builder()
            .from(from.parse().map_err(|e| format!("bad from address: {e}"))?)
            .to(to.parse().map_err(|e| format!("bad to address: {e}"))?)
            .subject(subject)
            .body(rendered.to_string())
            .map_err(|e| format!("building email: {e}"))?;

        // 465 = implicit TLS; otherwise STARTTLS (587).
        let builder = if port == 465 {
            SmtpTransport::relay(host)
        } else {
            SmtpTransport::starttls_relay(host)
        }
        .map_err(|e| format!("smtp setup: {e}"))?;
        let mailer = builder
            .port(port)
            .credentials(Credentials::new(username.to_string(), password.to_string()))
            .build();

        mailer
            .send(&message)
            .map(|_| ())
            .map_err(|e| format!("smtp send failed: {e}"))
    }
}

/// An email subject from the summary's first line.
#[cfg(feature = "email")]
fn email_subject(rendered: &str) -> String {
    let first = rendered
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Summary")
        .trim_start_matches('#')
        .trim();
    let snippet: String = first.chars().take(80).collect();
    format!("SummaryBot: {snippet}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::summarize::{ExtractedSummary, ResolvedCitation};
    use domain::MessageId;
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

    /// Records what it was asked to deliver, for a given kind id.
    struct SpyDeliverer {
        id: &'static str,
        sent: RefCell<Vec<String>>,
    }
    impl Deliverer for SpyDeliverer {
        fn id(&self) -> &str {
            self.id
        }
        fn deliver(&self, _config: &Value, rendered: &str) -> Result<(), String> {
            self.sent.borrow_mut().push(rendered.to_string());
            Ok(())
        }
    }

    fn cfg(kind: &str) -> ConfiguredDestination {
        ConfiguredDestination {
            dest: Destination::service(kind),
            config: serde_json::json!({ "url": "https://x" }),
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
            vec![("dashboard".to_string(), DeliveryOutcome::Stored)]
        );
        assert!(store.get_record(&ws(), "sum_1").unwrap().is_some());
    }

    #[test]
    fn allowed_service_destination_is_delivered_rendered() {
        let store = SqliteRepository::in_memory().unwrap();
        let deliverers: Vec<Box<dyn Deliverer>> = vec![Box::new(SpyDeliverer {
            id: "webhook",
            sent: RefCell::new(vec![]),
        })];
        let svc = DeliveryService::new(&store).with_deliverers(&deliverers);
        let caps = DeliveryCapabilities {
            enabled: vec!["webhook".into()],
            configured: vec!["webhook".into()],
            ..DeliveryCapabilities::default()
        };
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[cfg("webhook")],
                &caps,
                SummaryFormat::Markdown,
            )
            .unwrap();
        assert!(report
            .results
            .contains(&("webhook".to_string(), DeliveryOutcome::Delivered)));
        assert_eq!(report.delivered_count(), 2); // dashboard + webhook
    }

    #[test]
    fn unconfigured_service_destination_is_rejected() {
        let store = SqliteRepository::in_memory().unwrap();
        let svc = DeliveryService::new(&store);
        let caps = DeliveryCapabilities {
            enabled: vec!["webhook".into()], // enabled but not configured
            ..DeliveryCapabilities::default()
        };
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[cfg("webhook")],
                &caps,
                SummaryFormat::Markdown,
            )
            .unwrap();
        assert!(report.results.contains(&(
            "webhook".to_string(),
            DeliveryOutcome::Rejected(DeliveryReject::NotConfigured)
        )));
    }

    #[test]
    fn allowed_but_unregistered_destination_fails() {
        let store = SqliteRepository::in_memory().unwrap();
        let svc = DeliveryService::new(&store); // no deliverers registered
        let caps = DeliveryCapabilities {
            enabled: vec!["webhook".into()],
            configured: vec!["webhook".into()],
            ..DeliveryCapabilities::default()
        };
        let report = svc
            .deliver(
                &ws(),
                &record(),
                &[cfg("webhook")],
                &caps,
                SummaryFormat::Markdown,
            )
            .unwrap();
        let (_, outcome) = report.results.iter().find(|(k, _)| k == "webhook").unwrap();
        assert!(matches!(outcome, DeliveryOutcome::Failed(_)));
    }

    #[test]
    fn load_workspace_delivery_decrypts_enabled_and_marks_caps() {
        use repository::{DestinationRepository, StoredDestination};
        let master = [7u8; 32];
        let store = SqliteRepository::in_memory().unwrap();
        let enc = crate::encrypt_secret(&master, r#"{"url":"https://hooks.example/abc"}"#).unwrap();
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
                    address_enc: Some(crate::encrypt_secret(&master, r#"{"url":"off"}"#).unwrap()),
                    enabled: false,
                    created_at: 2,
                },
            )
            .unwrap();

        let (dests, caps) = load_workspace_delivery(&store, &ws(), Some(&master)).unwrap();
        assert_eq!(dests.len(), 1);
        assert_eq!(dests[0].dest.kind, "webhook");
        assert_eq!(
            dests[0].config.get("url").and_then(Value::as_str),
            Some("https://hooks.example/abc")
        );
        assert!(caps.enabled.iter().any(|k| k == "webhook"));
        assert!(caps.configured.iter().any(|k| k == "webhook"));
        assert_eq!(
            resolve_delivery(&dests[0].dest, &caps),
            DeliveryDecision::Allowed
        );
    }

    #[test]
    fn load_workspace_delivery_wraps_legacy_bare_url() {
        use repository::{DestinationRepository, StoredDestination};
        let master = [7u8; 32];
        let store = SqliteRepository::in_memory().unwrap();
        // Legacy row: a bare URL (not JSON).
        let enc = crate::encrypt_secret(&master, "https://legacy.example/hook").unwrap();
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
        let (dests, _) = load_workspace_delivery(&store, &ws(), Some(&master)).unwrap();
        assert_eq!(
            dests[0].config.get("url").and_then(Value::as_str),
            Some("https://legacy.example/hook")
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
        let (dests, caps) = load_workspace_delivery(&store, &ws(), None).unwrap();
        assert!(dests.is_empty());
        assert!(caps.enabled.is_empty());
    }

    #[cfg(feature = "email")]
    #[test]
    fn email_deliverer_rejects_incomplete_config_before_connecting() {
        let d = EmailDeliverer;
        assert_eq!(d.id(), "email");
        // Missing username/password/from/to → fails fast, no SMTP attempt.
        let err = d
            .deliver(
                &serde_json::json!({ "smtp_host": "smtp.example.com" }),
                "hi",
            )
            .unwrap_err();
        assert!(err.contains("incomplete"), "got: {err}");
        // A bad port is reported without connecting.
        let err = d
            .deliver(
                &serde_json::json!({
                    "smtp_host": "smtp.example.com",
                    "smtp_port": "not-a-number",
                    "username": "u",
                    "password": "p",
                    "from": "a@b.com",
                    "to": "c@d.com",
                }),
                "hi",
            )
            .unwrap_err();
        assert!(err.contains("invalid smtp_port"), "got: {err}");
    }
}

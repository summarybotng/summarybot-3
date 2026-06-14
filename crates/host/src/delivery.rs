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
use repository::{
    DestinationRepository, PlatformCredentialRepository, StructuredSummaryRepository, SummaryRecord,
    TenantPluginRepository, WorkspaceRepository,
};
use serde_json::Value;

/// A summary pre-rendered in every format, so each sink can pick the one it
/// wants (webhook → markdown/json, email/Confluence → html, …) without
/// re-rendering.
#[derive(Debug, Clone)]
pub struct RenderedSummary {
    pub markdown: String,
    pub plain: String,
    pub html: String,
    /// Structured fields, for machine-readable sinks (webhook `data`).
    pub data: Value,
}

impl RenderedSummary {
    /// Build all formats + structured data from a summary.
    pub fn new(summary: &domain::summarize::ExtractedSummary) -> Self {
        Self {
            markdown: render(summary, SummaryFormat::Markdown),
            plain: render(summary, SummaryFormat::Plain),
            html: render(summary, SummaryFormat::Html),
            data: summary_data(summary),
        }
    }

    /// A trivial bundle from one text (for test sends).
    pub fn from_text(text: &str) -> Self {
        Self {
            markdown: text.to_string(),
            plain: text.to_string(),
            html: format!("<p>{}</p>", text),
            data: serde_json::json!({ "text": text }),
        }
    }

    /// First non-empty line of the plain text — a natural title/subject.
    pub fn title(&self) -> &str {
        self.plain
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("Summary")
    }
}

/// Faithful structured JSON of a summary (for the webhook `data` field). Built
/// by hand so the pure domain type stays serialization-free.
fn summary_data(s: &domain::summarize::ExtractedSummary) -> Value {
    serde_json::json!({
        "text": s.text,
        "key_points": s.key_points,
        "action_items": s.action_items.iter().map(|a| serde_json::json!({
            "text": a.text,
            "assignee": a.assignee,
        })).collect::<Vec<_>>(),
        "technical_terms": s.technical_terms,
        "participants": s.participants,
        "citations": s.citations.iter().map(|c| serde_json::json!({
            "message_id": c.message_id.as_str(),
            "quote": c.quote,
        })).collect::<Vec<_>>(),
    })
}

/// A sink plugin: sends a rendered summary to one destination kind, given that
/// destination's decrypted JSON config. Sync for now (matches the codebase);
/// object-safe so plugins compose in a registry.
pub trait Deliverer {
    /// The destination kind id this plugin handles ("webhook", "confluence", …).
    fn id(&self) -> &str;
    /// Send `summary` (pre-rendered in all formats) using `config` (the decrypted
    /// per-destination JSON). `Err` is a transport/config failure message.
    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String>;
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

/// Which layer of the two-layer plugin model owns a config field (ADR-126).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldScope {
    /// Account-level credential/connection, set once by a tenant admin (e.g. a
    /// Confluence API token, SMTP creds, a Google Drive refresh token).
    Tenant,
    /// Per-destination target, set on a workspace destination (e.g. the channel
    /// id, Confluence space key, Drive folder, recipient, webhook URL).
    Workspace,
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
    /// Whether this field is configured at the tenant or workspace layer.
    pub scope: FieldScope,
}

/// A sink plugin's descriptor — drives API validation and the dashboard form.
#[derive(Debug, Clone)]
pub struct SinkDescriptor {
    pub id: &'static str,
    pub display_name: &'static str,
    pub fields: &'static [FieldSpec],
}

impl SinkDescriptor {
    /// Fields configured once at the tenant level (credentials/connection).
    pub fn tenant_fields(&self) -> impl Iterator<Item = &FieldSpec> {
        self.fields.iter().filter(|f| f.scope == FieldScope::Tenant)
    }
    /// Fields configured per workspace destination (the target).
    pub fn workspace_fields(&self) -> impl Iterator<Item = &FieldSpec> {
        self.fields
            .iter()
            .filter(|f| f.scope == FieldScope::Workspace)
    }
    /// Whether this plugin needs any tenant-level credentials at all.
    pub fn has_tenant_fields(&self) -> bool {
        self.fields.iter().any(|f| f.scope == FieldScope::Tenant)
    }
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
    #[cfg(feature = "gdrive")]
    out.push(GDRIVE_DESCRIPTOR);
    #[cfg(feature = "discord")]
    out.push(DISCORD_DESCRIPTOR);
    #[cfg(feature = "slack")]
    out.push(SLACK_DESCRIPTOR);
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
    #[cfg(feature = "gdrive")]
    out.push(Box::new(GoogleDriveDeliverer::default()));
    #[cfg(feature = "discord")]
    out.push(Box::new(DiscordChannelDeliverer::default()));
    #[cfg(feature = "slack")]
    out.push(Box::new(SlackChannelDeliverer::default()));
    out
}

/// The channel-send sinks (discord/slack) reuse the workspace's stored platform
/// bot token — the *same* credential used to fetch (ADR-128) — rather than
/// duplicating it in the destination config. This maps a sink kind to the
/// platform credential key it needs, or `None` for sinks that carry their own.
fn platform_token_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "discord" => Some("discord"),
        "slack" => Some("slack"),
        _ => None,
    }
}

/// Merge the decrypted platform bot token into a destination's config for the
/// channel-send sinks, so the deliverer is a pure function of (config, summary)
/// yet reuses the single stored credential. A no-op for other kinds, when there
/// is no master key, or when no credential is stored (the deliverer then fails
/// fast with a clear "no bot token" message).
pub fn inject_platform_token(
    repo: &impl PlatformCredentialRepository,
    workspace: &WorkspaceId,
    kind: &str,
    config: Value,
    master: Option<&[u8; 32]>,
) -> Value {
    let mut config = config;
    if let (Some(platform), Some(master)) = (platform_token_kind(kind), master) {
        if let Ok(Some(enc)) = repo.get_platform_token(workspace, platform) {
            if let Ok(token) = crate::decrypt_secret(master, &enc) {
                if let Some(obj) = config.as_object_mut() {
                    obj.insert("token".to_string(), Value::String(token));
                }
            }
        }
    }
    config
}

/// Overlay `overlay`'s keys onto `base` (overlay wins) when both are JSON objects.
/// Used to merge a workspace destination's target *over* the tenant credentials.
fn merge_under(base: Value, overlay: Value) -> Value {
    match (base, overlay) {
        (Value::Object(mut b), Value::Object(o)) => {
            for (k, v) in o {
                b.insert(k, v);
            }
            Value::Object(b)
        }
        (_, overlay) => overlay,
    }
}

/// Apply the per-tenant plugin layer to a workspace destination's config (ADR-126,
/// two-layer model): merge the tenant's stored account credentials *under* the
/// workspace target (target keys win). Returns `None` when the tenant has
/// explicitly disabled this plugin kind (the destination is then skipped).
///
/// A workspace with no tenant (dev/unprovisioned) or a kind the tenant has never
/// touched passes through unchanged — preserving the existing per-workspace
/// behavior and the dev/demo flow.
fn apply_tenant_layer(
    repo: &impl TenantPluginRepository,
    tenant: Option<&domain::TenantId>,
    kind: &str,
    ws_config: Value,
    master: Option<&[u8; 32]>,
) -> Option<Value> {
    let Some(tenant) = tenant else {
        return Some(ws_config); // dev/unprovisioned workspace: no tenant layer
    };
    match repo.get_tenant_plugin(tenant, kind) {
        Ok(Some(tp)) => {
            if tp.operator_disabled {
                return None; // platform-operator veto (ADR-131) — beats the tenant toggle
            }
            if !tp.enabled {
                return None; // explicit tenant disable → skip this destination
            }
            if let (Some(enc), Some(master)) = (tp.config_enc.as_deref(), master) {
                if let Ok(plain) = crate::decrypt_secret(master, enc) {
                    if let Ok(tenant_cfg) = serde_json::from_str::<Value>(&plain) {
                        return Some(merge_under(tenant_cfg, ws_config));
                    }
                }
            }
            Some(ws_config)
        }
        // No tenant row for this kind → default-on (backward-compatible).
        _ => Some(ws_config),
    }
}

/// Resolve a single destination's effective config exactly as a real delivery
/// would: apply the per-tenant plugin layer (merge credentials, honor an explicit
/// disable) then inject any per-workspace platform bot token. Returns `None` when
/// the tenant has disabled the plugin kind. Used by the test-send path so a test
/// exercises the same resolution as a live send.
pub fn resolve_destination_config(
    repo: &(impl PlatformCredentialRepository + WorkspaceRepository + TenantPluginRepository),
    workspace: &WorkspaceId,
    kind: &str,
    config: Value,
    master: Option<&[u8; 32]>,
) -> Option<Value> {
    let tenant = repo.find_workspace(workspace).ok().flatten().map(|w| w.tenant_id);
    let config = apply_tenant_layer(repo, tenant.as_ref(), kind, config, master)?;
    Some(inject_platform_token(repo, workspace, kind, config, master))
}

/// A gating destination paired with its decrypted config, ready to dispatch.
pub struct ConfiguredDestination {
    /// Stored destination id (so a schedule can restrict delivery to a subset).
    pub id: String,
    pub dest: Destination,
    pub config: Value,
    /// ADR-108: deliver here on every rolling run, not just at finalize.
    pub rolling_deliver_intermediate: bool,
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
    ) -> anyhow::Result<DeliveryReport> {
        let mut results = Vec::new();

        // Always-on dashboard store (PRD §4 item 3).
        self.store.save_record(workspace, record)?;
        results.push(("dashboard".to_string(), DeliveryOutcome::Stored));

        let rendered = RenderedSummary::new(&record.summary);
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

    /// Deliver to external destinations **without** the dashboard store — for
    /// intermediate rolling-period updates (ADR-108), where the in-progress
    /// digest goes out to flagged destinations on each run but shouldn't litter
    /// the dashboard with a record every time (the finalized digest is stored).
    pub fn deliver_external(
        &self,
        record: &SummaryRecord,
        destinations: &[ConfiguredDestination],
        caps: &DeliveryCapabilities,
    ) -> DeliveryReport {
        let mut results = Vec::new();
        let rendered = RenderedSummary::new(&record.summary);
        for cd in destinations {
            if cd.dest.class == DeliveryClass::Dashboard {
                continue;
            }
            let outcome = match resolve_delivery(&cd.dest, caps) {
                DeliveryDecision::Rejected(reason) => DeliveryOutcome::Rejected(reason),
                DeliveryDecision::Allowed => self.dispatch(&cd.dest.kind, &cd.config, &rendered),
            };
            results.push((cd.dest.kind.clone(), outcome));
        }
        DeliveryReport { results }
    }

    fn dispatch(&self, kind: &str, config: &Value, summary: &RenderedSummary) -> DeliveryOutcome {
        match self.deliverers.iter().find(|d| d.id() == kind) {
            Some(d) => match d.deliver(config, summary) {
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
    repo: &(impl DestinationRepository
          + PlatformCredentialRepository
          + WorkspaceRepository
          + TenantPluginRepository),
    workspace: &WorkspaceId,
    master: Option<&[u8; 32]>,
) -> anyhow::Result<(Vec<ConfiguredDestination>, DeliveryCapabilities)> {
    // The workspace's tenant, if any — drives the per-tenant plugin layer below.
    // A workspace with no stored row (dev/unprovisioned) has no tenant layer.
    let tenant = repo.find_workspace(workspace)?.map(|w| w.tenant_id);
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
        // Per-tenant plugin layer (ADR-126): merge the tenant's account credentials
        // under this destination's target, and honor an explicit tenant disable.
        let config = match apply_tenant_layer(repo, tenant.as_ref(), &row.kind, config, master) {
            Some(c) => c,
            None => continue, // the tenant has disabled this plugin kind
        };
        // Channel-send sinks borrow the workspace's stored platform bot token.
        let config = inject_platform_token(repo, workspace, &row.kind, config, master);
        if !caps.enabled.contains(&row.kind) {
            caps.enabled.push(row.kind.clone());
        }
        if !caps.configured.contains(&row.kind) {
            caps.configured.push(row.kind.clone());
        }
        destinations.push(ConfiguredDestination {
            id: row.id.clone(),
            dest: Destination::service(row.kind),
            config,
            rolling_deliver_intermediate: row.rolling_deliver_intermediate,
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
        scope: FieldScope::Workspace,
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

    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String> {
        let url = config
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| "webhook destination has no url".to_string())?;
        // `text`/`content` satisfy Slack/Discord; `summary`/`html`/`data` are
        // extras for generic receivers (`data` is the machine-readable summary).
        let body = serde_json::json!({
            "text": summary.markdown,
            "content": summary.markdown,
            "summary": summary.markdown,
            "html": summary.html,
            "data": summary.data,
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

// ---- Confluence sink plugin (ADR-126; OAuth ADR-132, legacy ADR-099) -------

/// Confluence Cloud config. The primary path is **Atlassian OAuth** (ADR-132):
/// the tenant clicks Connect and a `refresh_token` + `cloud_id` are captured.
/// The legacy `base_url`/`email`/`api_token` Basic-auth fields remain (now
/// optional) so destinations configured before OAuth keep delivering. `space_key`
/// is the per-workspace target either way.
#[cfg(feature = "confluence")]
const CONFLUENCE_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "confluence",
    display_name: "Confluence",
    fields: &[
        FieldSpec {
            name: "base_url",
            label: "Base URL (legacy API-token auth; leave blank if connecting via OAuth)",
            secret: false,
            required: false,
            hint: FieldHint::Full,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "space_key",
            label: "Space key",
            secret: false,
            required: true,
            hint: FieldHint::Full,
            scope: FieldScope::Workspace,
        },
        FieldSpec {
            name: "email",
            label: "Account email (legacy API-token auth)",
            secret: false,
            required: false,
            hint: FieldHint::Full,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "api_token",
            label: "API token (legacy API-token auth)",
            secret: true,
            required: false,
            hint: FieldHint::None,
            scope: FieldScope::Tenant,
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

    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String> {
        let field = |k: &str| config.get(k).and_then(Value::as_str).unwrap_or("").trim();
        let space_key = field("space_key");
        if space_key.is_empty() {
            return Err("confluence destination has no space_key".to_string());
        }
        let body = serde_json::json!({
            "type": "page",
            "title": confluence_title(summary.title()),
            "space": { "key": space_key },
            // Our HTML render is valid Confluence storage-format XHTML.
            "body": { "storage": { "value": summary.html, "representation": "storage" } }
        });

        // Primary path (ADR-132): OAuth — refresh to an access token and POST to
        // the Cloud API at api.atlassian.com/ex/confluence/{cloud_id}.
        let refresh_token = field("refresh_token");
        let cloud_id = field("cloud_id");
        if !refresh_token.is_empty() && !cloud_id.is_empty() {
            let endpoint =
                format!("https://api.atlassian.com/ex/confluence/{cloud_id}/wiki/rest/api/content");
            let access = self.atlassian_access_token(refresh_token)?;
            return self.post_page(&endpoint, &format!("Bearer {access}"), body);
        }

        // Legacy fallback: Basic auth against the tenant's own base URL.
        let base_url = field("base_url").trim_end_matches('/');
        let email = field("email");
        let token = field("api_token");
        if base_url.is_empty() || email.is_empty() || token.is_empty() {
            return Err(
                "confluence destination is not connected — connect via OAuth, or set base_url + email + api_token"
                    .to_string(),
            );
        }
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let auth = format!("Basic {}", STANDARD.encode(format!("{email}:{token}")));
        let endpoint = format!("{base_url}/wiki/rest/api/content");
        self.post_page(&endpoint, &auth, body)
    }
}

#[cfg(feature = "confluence")]
impl ConfluenceDeliverer {
    /// POST a storage-format page to a Confluence content endpoint with the given
    /// `Authorization` header (Bearer for OAuth, Basic for legacy).
    fn post_page(&self, endpoint: &str, authorization: &str, body: Value) -> Result<(), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        match agent
            .post(endpoint)
            .set("Authorization", authorization)
            .set("Content-Type", "application/json")
            .send_json(body)
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(format!("confluence returned http {code}")),
            Err(ureq::Error::Transport(t)) => Err(format!("confluence transport error: {t}")),
        }
    }

    /// Exchange the stored Atlassian refresh token for an access token (ADR-132),
    /// using the operator's Atlassian OAuth app from the environment.
    fn atlassian_access_token(&self, refresh_token: &str) -> Result<String, String> {
        let (provider, secret) = atlassian_oauth_app()?;
        Ok(crate::oauth::refresh(&provider, &secret, refresh_token)?.access_token)
    }
}

/// The operator's Atlassian OAuth app (client id/secret) from the environment.
/// Shared by the Connect callback and the deliverer's token refresh (ADR-132).
#[cfg(feature = "confluence")]
pub fn atlassian_oauth_app() -> Result<(domain::OAuthProvider, String), String> {
    let client_id = std::env::var("ATLASSIAN_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "server has no ATLASSIAN_CLIENT_ID configured".to_string())?;
    let client_secret = std::env::var("ATLASSIAN_CLIENT_SECRET")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "server has no ATLASSIAN_CLIENT_SECRET configured".to_string())?;
    Ok((
        domain::OAuthProvider {
            name: "atlassian".into(),
            auth_url: "https://auth.atlassian.com/authorize".into(),
            token_url: "https://auth.atlassian.com/oauth/token".into(),
            userinfo_url: None,
            client_id,
            scopes: vec![
                "write:confluence-content".into(),
                "read:confluence-space.summary".into(),
                "offline_access".into(),
            ],
            extra_auth_params: vec![
                ("audience".into(), "api.atlassian.com".into()),
                ("prompt".into(), "consent".into()),
            ],
        },
        client_secret,
    ))
}

/// A unique page title (Confluence titles are unique per space) from a snippet
/// plus a clock suffix.
#[cfg(feature = "confluence")]
fn confluence_title(first_line: &str) -> String {
    let snippet: String = first_line.trim().chars().take(80).collect();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("SummaryBot: {snippet} ({ts})")
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
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "smtp_port",
            label: "SMTP port (587 STARTTLS / 465 TLS)",
            secret: false,
            required: false,
            hint: FieldHint::Full,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "username",
            label: "Username",
            secret: false,
            required: true,
            hint: FieldHint::Full,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "password",
            label: "Password / app password",
            secret: true,
            required: true,
            hint: FieldHint::None,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "from",
            label: "From address",
            secret: false,
            required: true,
            hint: FieldHint::Full,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "to",
            label: "To address",
            secret: false,
            required: true,
            hint: FieldHint::Full,
            scope: FieldScope::Workspace,
        },
    ],
};

/// Send each summary as a multipart (plain + HTML) email over SMTP (DEL-004).
/// Port 465 uses implicit TLS; anything else (default 587) uses STARTTLS. Single
/// recipient for now — multiple recipients are a later refinement.
#[cfg(feature = "email")]
pub struct EmailDeliverer;

#[cfg(feature = "email")]
impl Deliverer for EmailDeliverer {
    fn id(&self) -> &str {
        "email"
    }

    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String> {
        use lettre::message::{header::ContentType, MultiPart, SinglePart};
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

        let html_doc = format!("<!doctype html><html><body>{}</body></html>", summary.html);
        let body = MultiPart::alternative()
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_PLAIN)
                    .body(summary.plain.clone()),
            )
            .singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(html_doc),
            );
        let message = Message::builder()
            .from(from.parse().map_err(|e| format!("bad from address: {e}"))?)
            .to(to.parse().map_err(|e| format!("bad to address: {e}"))?)
            .subject(email_subject(summary.title()))
            .multipart(body)
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

/// An email subject from the summary's title line.
#[cfg(feature = "email")]
fn email_subject(title: &str) -> String {
    let snippet: String = title.trim().chars().take(80).collect();
    format!("SummaryBot: {snippet}")
}

// ---- Google Drive sink plugin (ADR-126) ------------------------------------

/// Google Drive config: a per-workspace refresh token (obtained via the Google
/// OAuth consent with the `drive.file` scope) and an optional destination folder.
/// The operator's OAuth *app* creds come from the process env
/// (`GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET`).
#[cfg(feature = "gdrive")]
const GDRIVE_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "gdrive",
    display_name: "Google Drive",
    fields: &[
        FieldSpec {
            name: "refresh_token",
            label: "Google OAuth refresh token (drive.file scope)",
            secret: true,
            required: true,
            hint: FieldHint::None,
            scope: FieldScope::Tenant,
        },
        FieldSpec {
            name: "folder_id",
            label: "Destination folder id (optional)",
            secret: false,
            required: false,
            hint: FieldHint::Full,
            scope: FieldScope::Workspace,
        },
    ],
};

/// Publish each summary as a Google Doc (DEL via Drive). Refreshes a short-lived
/// access token from the stored refresh token (operator app creds from env),
/// then multipart-uploads the HTML render with `mimeType
/// application/vnd.google-apps.document` so Drive converts it to a Doc.
#[cfg(feature = "gdrive")]
pub struct GoogleDriveDeliverer {
    timeout_secs: u64,
}

#[cfg(feature = "gdrive")]
impl Default for GoogleDriveDeliverer {
    fn default() -> Self {
        Self { timeout_secs: 30 }
    }
}

#[cfg(feature = "gdrive")]
impl Deliverer for GoogleDriveDeliverer {
    fn id(&self) -> &str {
        "gdrive"
    }

    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String> {
        let refresh_token = config
            .get("refresh_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| "google drive destination has no refresh token".to_string())?;
        let folder_id = config
            .get("folder_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();

        // Operator's Google OAuth app (shared with login).
        let client_id = std::env::var("GOOGLE_CLIENT_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "server has no GOOGLE_CLIENT_ID configured".to_string())?;
        let client_secret = std::env::var("GOOGLE_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "server has no GOOGLE_CLIENT_SECRET configured".to_string())?;
        let provider = domain::OAuthProvider {
            name: "google".into(),
            auth_url: String::new(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: None,
            client_id,
            scopes: vec![],
            extra_auth_params: vec![],
        };
        let access = crate::oauth::refresh(&provider, &client_secret, refresh_token)?.access_token;

        // multipart/related: JSON metadata + HTML media → a converted Google Doc.
        let boundary = format!("sbnd{}", crate::oauth::random_url_token(12)?);
        let mut metadata = serde_json::json!({
            "name": format!("SummaryBot: {}", summary.title()),
            "mimeType": "application/vnd.google-apps.document",
        });
        if !folder_id.is_empty() {
            metadata["parents"] = serde_json::json!([folder_id]);
        }
        let html_doc = format!("<html><body>{}</body></html>", summary.html);
        let body = format!(
            "--{b}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n\
             --{b}\r\nContent-Type: text/html; charset=UTF-8\r\n\r\n{html}\r\n--{b}--",
            b = boundary,
            meta = metadata,
            html = html_doc,
        );
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        match agent
            .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart")
            .set("Authorization", &format!("Bearer {access}"))
            .set(
                "Content-Type",
                &format!("multipart/related; boundary={boundary}"),
            )
            .send_bytes(body.as_bytes())
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(format!("google drive returned http {code}")),
            Err(ureq::Error::Transport(t)) => Err(format!("google drive transport error: {t}")),
        }
    }
}

// ---- Discord channel send-back sink (ADR-126/128; DEL-010) -----------------

/// Discord config: the target channel id. The bot token is the workspace's
/// stored Discord credential (injected at delivery time), not a separate secret.
#[cfg(feature = "discord")]
const DISCORD_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "discord",
    display_name: "Discord channel",
    fields: &[FieldSpec {
        name: "channel",
        label: "Channel ID (right-click a channel → Copy Channel ID)",
        secret: false,
        required: true,
        hint: FieldHint::Full,
        scope: FieldScope::Workspace,
    }],
};

/// Post each summary back to a Discord channel (the loop's send half). Reuses the
/// workspace's stored bot token (ADR-128) via `inject_platform_token`; the config
/// carries only the channel id. Discord caps message content at 2000 chars, so
/// the markdown is truncated with an ellipsis marker.
#[cfg(feature = "discord")]
pub struct DiscordChannelDeliverer {
    timeout_secs: u64,
}

#[cfg(feature = "discord")]
impl Default for DiscordChannelDeliverer {
    fn default() -> Self {
        Self { timeout_secs: 15 }
    }
}

#[cfg(feature = "discord")]
impl Deliverer for DiscordChannelDeliverer {
    fn id(&self) -> &str {
        "discord"
    }

    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String> {
        let channel = config
            .get("channel")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| "discord destination has no channel id".to_string())?;
        let token = config
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                "no Discord bot token set for this workspace (add it under the Discord source)"
                    .to_string()
            })?;
        let content = truncate_chars(&summary.markdown, 2000);
        let url = format!("https://discord.com/api/v10/channels/{channel}/messages");
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        match agent
            .post(&url)
            .set("Authorization", &format!("Bot {token}"))
            .send_json(serde_json::json!({ "content": content }))
        {
            Ok(_) => Ok(()),
            Err(ureq::Error::Status(code, _)) => Err(format!("discord returned http {code}")),
            Err(ureq::Error::Transport(t)) => Err(format!("discord transport error: {t}")),
        }
    }
}

/// Truncate to at most `max` characters (not bytes), appending an ellipsis when
/// cut, so multi-byte content can't split a codepoint or blow a char limit.
#[cfg(feature = "discord")]
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

// ---- Slack channel send-back sink (ADR-126/128; DEL-010) -------------------

/// Slack config: the target channel id. The bot token is the workspace's stored
/// Slack credential (injected at delivery time).
#[cfg(feature = "slack")]
const SLACK_DESCRIPTOR: SinkDescriptor = SinkDescriptor {
    id: "slack",
    display_name: "Slack channel",
    fields: &[FieldSpec {
        name: "channel",
        label: "Channel ID (e.g. C0123ABCD) or #name",
        secret: false,
        required: true,
        hint: FieldHint::Full,
        scope: FieldScope::Workspace,
    }],
};

/// Post each summary back to a Slack channel via `chat.postMessage`. Reuses the
/// workspace's stored bot token (ADR-128). Slack signals failure in the JSON body
/// (`{"ok": false, "error": …}`) with HTTP 200, so the body is checked.
#[cfg(feature = "slack")]
pub struct SlackChannelDeliverer {
    timeout_secs: u64,
}

#[cfg(feature = "slack")]
impl Default for SlackChannelDeliverer {
    fn default() -> Self {
        Self { timeout_secs: 15 }
    }
}

#[cfg(feature = "slack")]
impl Deliverer for SlackChannelDeliverer {
    fn id(&self) -> &str {
        "slack"
    }

    fn deliver(&self, config: &Value, summary: &RenderedSummary) -> Result<(), String> {
        let channel = config
            .get("channel")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| "slack destination has no channel id".to_string())?;
        let token = config
            .get("token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                "no Slack bot token set for this workspace (add it under the Slack source)"
                    .to_string()
            })?;
        let agent = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .build();
        let resp = agent
            .post("https://slack.com/api/chat.postMessage")
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(serde_json::json!({ "channel": channel, "text": summary.markdown }));
        let body: Value = match resp {
            Ok(r) => r.into_json().map_err(|e| format!("slack bad response: {e}"))?,
            Err(ureq::Error::Status(code, _)) => return Err(format!("slack returned http {code}")),
            Err(ureq::Error::Transport(t)) => return Err(format!("slack transport error: {t}")),
        };
        if body.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(())
        } else {
            let err = body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            Err(format!("slack rejected the message: {err}"))
        }
    }
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
            coherence_score: None,
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
        fn deliver(&self, _config: &Value, summary: &RenderedSummary) -> Result<(), String> {
            self.sent.borrow_mut().push(summary.markdown.clone());
            Ok(())
        }
    }

    fn cfg(kind: &str) -> ConfiguredDestination {
        ConfiguredDestination {
            id: format!("d-{kind}"),
            dest: Destination::service(kind),
            config: serde_json::json!({ "url": "https://x" }),
            rolling_deliver_intermediate: false,
        }
    }

    #[cfg(feature = "confluence")]
    #[test]
    fn confluence_deliver_selects_path_and_guards_config() {
        // ADR-132: space_key is required; with neither OAuth (refresh_token +
        // cloud_id) nor legacy Basic (base_url + email + api_token) creds, the
        // deliver fails fast with a clear message before any network call.
        let d = ConfluenceDeliverer::default();
        let r = RenderedSummary::new(&record().summary);

        let missing_space = d.deliver(&serde_json::json!({}), &r).unwrap_err();
        assert!(missing_space.contains("space_key"), "got: {missing_space}");

        let not_connected = d
            .deliver(&serde_json::json!({ "space_key": "ENG" }), &r)
            .unwrap_err();
        assert!(
            not_connected.contains("connect") || not_connected.contains("api_token"),
            "expected a 'not connected' hint, got: {not_connected}"
        );
    }

    #[test]
    fn rendered_bundle_carries_structured_data() {
        let r = RenderedSummary::new(&record().summary);
        assert_eq!(r.data["text"], "We shipped.");
        assert_eq!(r.data["key_points"][0], "Launched");
        assert_eq!(r.data["participants"][0], "Alice");
        assert_eq!(r.data["citations"][0]["message_id"], "m0");
        // Title comes from the plain render's first line.
        assert_eq!(r.title(), "We shipped.");
    }

    #[test]
    fn always_stores_to_dashboard_even_with_no_destinations() {
        let store = SqliteRepository::in_memory().unwrap();
        let svc = DeliveryService::new(&store);
        let report = svc
            .deliver(&ws(), &record(), &[], &DeliveryCapabilities::default())
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
            .deliver(&ws(), &record(), &[cfg("webhook")], &caps)
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
            .deliver(&ws(), &record(), &[cfg("webhook")], &caps)
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
            .deliver(&ws(), &record(), &[cfg("webhook")], &caps)
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
                    rolling_deliver_intermediate: false,
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
                    rolling_deliver_intermediate: false,
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
                    rolling_deliver_intermediate: false,
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
    fn tenant_layer_merges_credentials_under_workspace_target_and_gates() {
        use repository::{
            DestinationRepository, StoredDestination, TenantPlugin, TenantPluginRepository,
            WorkspaceRepository,
        };
        let master = [5u8; 32];
        let store = SqliteRepository::in_memory().unwrap();
        let tenant = domain::TenantId::parse("acme").unwrap();
        // A provisioned workspace under the tenant.
        store
            .create_workspace(
                &domain::Workspace::create(
                    ws(),
                    tenant.clone(),
                    "Eng",
                    domain::UserId::parse("u1").unwrap(),
                    1,
                )
                .unwrap(),
            )
            .unwrap();
        // Tenant enables Confluence + stores account credentials.
        let creds = crate::encrypt_secret(
            &master,
            r#"{"base_url":"https://acme.atlassian.net","email":"a@b.com","api_token":"sekret"}"#,
        )
        .unwrap();
        store
            .upsert_tenant_plugin(
                &tenant,
                &TenantPlugin {
                    kind: "confluence".into(),
                    enabled: true,
                    config_enc: Some(creds),
                    connected: false,
                    updated_at: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        // Workspace destination carries only the target (space key).
        let target = crate::encrypt_secret(&master, r#"{"space_key":"ENG"}"#).unwrap();
        store
            .upsert_destination(
                &ws(),
                &StoredDestination {
                    id: "d1".into(),
                    kind: "confluence".into(),
                    address_enc: Some(target),
                    enabled: true,
                    created_at: 1,
                    rolling_deliver_intermediate: false,
                },
            )
            .unwrap();

        let (dests, _) = load_workspace_delivery(&store, &ws(), Some(&master)).unwrap();
        assert_eq!(dests.len(), 1);
        let cfg = &dests[0].config;
        // Tenant creds merged in, workspace target preserved.
        assert_eq!(cfg.get("base_url").and_then(Value::as_str), Some("https://acme.atlassian.net"));
        assert_eq!(cfg.get("api_token").and_then(Value::as_str), Some("sekret"));
        assert_eq!(cfg.get("space_key").and_then(Value::as_str), Some("ENG"));

        // Disabling the plugin at the tenant level skips the destination entirely.
        store
            .upsert_tenant_plugin(
                &tenant,
                &TenantPlugin {
                    kind: "confluence".into(),
                    enabled: false,
                    config_enc: None,
                    connected: false,
                    updated_at: 2,
                    ..Default::default()
                },
            )
            .unwrap();
        let (dests, _) = load_workspace_delivery(&store, &ws(), Some(&master)).unwrap();
        assert!(dests.is_empty());
    }

    #[test]
    fn operator_disabled_vetoes_a_plugin_even_when_the_tenant_enabled_it() {
        // ADR-131: the operator veto beats the tenant's own enablement.
        use repository::{TenantPlugin, TenantPluginRepository};
        let store = SqliteRepository::in_memory().unwrap();
        let tenant = domain::TenantId::parse("acme").unwrap();
        store
            .upsert_tenant_plugin(
                &tenant,
                &TenantPlugin {
                    kind: "webhook".into(),
                    enabled: true,          // tenant wants it on
                    operator_disabled: true, // operator vetoes
                    updated_at: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        let cfg = serde_json::json!({ "url": "https://x" });
        // Vetoed → skipped.
        assert!(apply_tenant_layer(&store, Some(&tenant), "webhook", cfg.clone(), None).is_none());
        // A different, un-vetoed kind with no row → default-on (passes through).
        assert!(apply_tenant_layer(&store, Some(&tenant), "email", cfg, None).is_some());
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
                    rolling_deliver_intermediate: false,
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
        let sample = RenderedSummary::from_text("hi");
        // Missing username/password/from/to → fails fast, no SMTP attempt.
        let err = d
            .deliver(
                &serde_json::json!({ "smtp_host": "smtp.example.com" }),
                &sample,
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
                &sample,
            )
            .unwrap_err();
        assert!(err.contains("invalid smtp_port"), "got: {err}");
    }

    #[cfg(feature = "discord")]
    #[test]
    fn discord_deliverer_needs_channel_then_token() {
        let d = DiscordChannelDeliverer::default();
        assert_eq!(d.id(), "discord");
        let sample = RenderedSummary::from_text("hi");
        // No channel → fails before any network.
        let err = d.deliver(&serde_json::json!({}), &sample).unwrap_err();
        assert!(err.contains("channel"), "got: {err}");
        // Channel but no injected token → clear, actionable error (no send).
        let err = d
            .deliver(&serde_json::json!({ "channel": "123" }), &sample)
            .unwrap_err();
        assert!(err.contains("bot token"), "got: {err}");
    }

    #[cfg(feature = "discord")]
    #[test]
    fn discord_truncates_to_the_2000_char_limit() {
        let long = "x".repeat(5000);
        let out = truncate_chars(&long, 2000);
        assert_eq!(out.chars().count(), 2000);
        assert!(out.ends_with('…'));
        // Short content is unchanged.
        assert_eq!(truncate_chars("short", 2000), "short");
    }

    #[cfg(feature = "slack")]
    #[test]
    fn slack_deliverer_needs_channel_then_token() {
        let d = SlackChannelDeliverer::default();
        assert_eq!(d.id(), "slack");
        let sample = RenderedSummary::from_text("hi");
        let err = d.deliver(&serde_json::json!({}), &sample).unwrap_err();
        assert!(err.contains("channel"), "got: {err}");
        let err = d
            .deliver(&serde_json::json!({ "channel": "C1" }), &sample)
            .unwrap_err();
        assert!(err.contains("bot token"), "got: {err}");
    }

    #[cfg(any(feature = "discord", feature = "slack"))]
    #[test]
    fn inject_platform_token_merges_stored_credential() {
        use repository::PlatformCredentialRepository;
        let master = [3u8; 32];
        let store = SqliteRepository::in_memory().unwrap();
        let enc = crate::encrypt_secret(&master, "bot-secret-xyz").unwrap();
        store
            .set_platform_token(&ws(), "discord", &enc, 1)
            .unwrap();
        // discord kind → token merged in.
        let cfg = inject_platform_token(
            &store,
            &ws(),
            "discord",
            serde_json::json!({ "channel": "123" }),
            Some(&master),
        );
        assert_eq!(cfg.get("token").and_then(Value::as_str), Some("bot-secret-xyz"));
        // A non-channel sink (webhook) is left untouched.
        let cfg = inject_platform_token(
            &store,
            &ws(),
            "webhook",
            serde_json::json!({ "url": "https://x" }),
            Some(&master),
        );
        assert!(cfg.get("token").is_none());
    }

    #[cfg(feature = "gdrive")]
    #[test]
    fn gdrive_deliverer_rejects_missing_refresh_token() {
        let d = GoogleDriveDeliverer::default();
        assert_eq!(d.id(), "gdrive");
        let err = d
            .deliver(
                &serde_json::json!({ "folder_id": "abc" }),
                &RenderedSummary::from_text("hi"),
            )
            .unwrap_err();
        assert!(err.contains("refresh token"), "got: {err}");
    }
}

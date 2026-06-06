//! Delivery destinations + capability gating (PRD §4, §4.3; DEL-010/011, DEN-*).
//!
//! Pure policy for *where* a summary may be sent. The dashboard store is
//! **always-on**; every other destination is **capability-gated**: it must be
//! admin-enabled and configured for the workspace, and platform destinations
//! must resolve against an actual connection (DEL-010). Gating is enforced here,
//! server-side (DEL-011) — the UI merely hides what [`visible_kinds`] omits. The
//! actual send is host I/O (a per-destination deliverer); this only decides
//! whether it's allowed.

use crate::Platform;

/// A kind of place a summary can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DestinationKind {
    /// Always-on stored-summary destination (the dashboard reads it).
    Dashboard,
    /// A channel on a connected platform (DEL-010).
    PlatformChannel,
    /// A DM on a connected platform (DEL-010).
    PlatformDm,
    /// Email (capability-gated, DEN-*).
    Email,
    /// Outbound webhook (capability-gated, DEN-*).
    Webhook,
}

impl DestinationKind {
    /// Whether the platform-connection check applies (DEL-010).
    fn needs_platform(self) -> bool {
        matches!(
            self,
            DestinationKind::PlatformChannel | DestinationKind::PlatformDm
        )
    }
}

/// A concrete destination instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub kind: DestinationKind,
    /// Required for platform destinations.
    pub platform: Option<Platform>,
    /// Channel id / DM recipient / email / webhook URL (opaque, validated by the
    /// host adapter). Not required for the dashboard.
    pub address: Option<String>,
}

/// Why a destination was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryReject {
    /// Not admin-enabled for this workspace.
    NotEnabled,
    /// Enabled but missing its (encrypted) config — hidden in the UI (DEN-*).
    NotConfigured,
    /// DEL-010: the workspace has no connection for the target platform.
    PlatformNotConnected,
    /// A platform destination didn't name a platform.
    PlatformMissing,
    /// The address (channel/email/url) is required but absent.
    MissingAddress,
}

/// The gating decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryDecision {
    Allowed,
    Rejected(DeliveryReject),
}

/// What a workspace can deliver to: its connected platforms, which destination
/// kinds an admin has enabled, and which have their config present.
#[derive(Debug, Clone, Default)]
pub struct DeliveryCapabilities {
    pub connected_platforms: Vec<Platform>,
    pub enabled: Vec<DestinationKind>,
    pub configured: Vec<DestinationKind>,
}

impl DeliveryCapabilities {
    fn is_enabled(&self, kind: DestinationKind) -> bool {
        self.enabled.contains(&kind)
    }
    fn is_configured(&self, kind: DestinationKind) -> bool {
        self.configured.contains(&kind)
    }
    fn is_connected(&self, platform: Platform) -> bool {
        self.connected_platforms.contains(&platform)
    }
}

/// Decide whether `dest` may receive a summary given the workspace `caps`.
pub fn resolve(dest: &Destination, caps: &DeliveryCapabilities) -> DeliveryDecision {
    // The dashboard store is always available (PRD §4 item 3).
    if dest.kind == DestinationKind::Dashboard {
        return DeliveryDecision::Allowed;
    }
    if !caps.is_enabled(dest.kind) {
        return DeliveryDecision::Rejected(DeliveryReject::NotEnabled);
    }
    if dest.kind.needs_platform() {
        let Some(platform) = dest.platform else {
            return DeliveryDecision::Rejected(DeliveryReject::PlatformMissing);
        };
        if !caps.is_connected(platform) {
            return DeliveryDecision::Rejected(DeliveryReject::PlatformNotConnected);
        }
    } else if !caps.is_configured(dest.kind) {
        // Email/webhook need their encrypted config present (DEN-*).
        return DeliveryDecision::Rejected(DeliveryReject::NotConfigured);
    }
    if dest.address.as_deref().is_none_or(str::is_empty) {
        return DeliveryDecision::Rejected(DeliveryReject::MissingAddress);
    }
    DeliveryDecision::Allowed
}

/// Destination kinds to surface in the UI (DEN-*: hidden when unconfigured).
/// Always includes the dashboard; platform kinds appear when a platform is
/// connected; email/webhook appear only when enabled **and** configured.
pub fn visible_kinds(caps: &DeliveryCapabilities) -> Vec<DestinationKind> {
    let mut kinds = vec![DestinationKind::Dashboard];
    if !caps.connected_platforms.is_empty() {
        for k in [
            DestinationKind::PlatformChannel,
            DestinationKind::PlatformDm,
        ] {
            if caps.is_enabled(k) {
                kinds.push(k);
            }
        }
    }
    for k in [DestinationKind::Email, DestinationKind::Webhook] {
        if caps.is_enabled(k) && caps.is_configured(k) {
            kinds.push(k);
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(
        kind: DestinationKind,
        platform: Option<Platform>,
        address: Option<&str>,
    ) -> Destination {
        Destination {
            kind,
            platform,
            address: address.map(str::to_string),
        }
    }

    #[test]
    fn dashboard_is_always_allowed_even_with_no_capabilities() {
        let caps = DeliveryCapabilities::default();
        assert_eq!(
            resolve(&dest(DestinationKind::Dashboard, None, None), &caps),
            DeliveryDecision::Allowed
        );
    }

    #[test]
    fn platform_channel_requires_a_connection() {
        let caps = DeliveryCapabilities {
            connected_platforms: vec![Platform::Slack],
            enabled: vec![DestinationKind::PlatformChannel],
            configured: vec![],
        };
        // Discord not connected → rejected (DEL-010).
        assert_eq!(
            resolve(
                &dest(
                    DestinationKind::PlatformChannel,
                    Some(Platform::Discord),
                    Some("c1")
                ),
                &caps
            ),
            DeliveryDecision::Rejected(DeliveryReject::PlatformNotConnected)
        );
        // Slack connected + address → allowed.
        assert_eq!(
            resolve(
                &dest(
                    DestinationKind::PlatformChannel,
                    Some(Platform::Slack),
                    Some("c1")
                ),
                &caps
            ),
            DeliveryDecision::Allowed
        );
    }

    #[test]
    fn disabled_destination_is_rejected_server_side() {
        let caps = DeliveryCapabilities::default(); // nothing enabled
        assert_eq!(
            resolve(&dest(DestinationKind::Email, None, Some("a@b.com")), &caps),
            DeliveryDecision::Rejected(DeliveryReject::NotEnabled)
        );
    }

    #[test]
    fn enabled_but_unconfigured_destination_is_rejected() {
        let caps = DeliveryCapabilities {
            connected_platforms: vec![],
            enabled: vec![DestinationKind::Webhook],
            configured: vec![], // no config
        };
        assert_eq!(
            resolve(
                &dest(DestinationKind::Webhook, None, Some("https://x")),
                &caps
            ),
            DeliveryDecision::Rejected(DeliveryReject::NotConfigured)
        );
    }

    #[test]
    fn configured_email_needs_an_address() {
        let caps = DeliveryCapabilities {
            connected_platforms: vec![],
            enabled: vec![DestinationKind::Email],
            configured: vec![DestinationKind::Email],
        };
        assert_eq!(
            resolve(&dest(DestinationKind::Email, None, None), &caps),
            DeliveryDecision::Rejected(DeliveryReject::MissingAddress)
        );
        assert_eq!(
            resolve(&dest(DestinationKind::Email, None, Some("a@b.com")), &caps),
            DeliveryDecision::Allowed
        );
    }

    #[test]
    fn visible_kinds_hides_unconfigured_and_unconnected() {
        // Nothing connected/configured → only the dashboard shows.
        let bare = DeliveryCapabilities {
            enabled: vec![DestinationKind::PlatformChannel, DestinationKind::Email],
            ..DeliveryCapabilities::default()
        };
        assert_eq!(visible_kinds(&bare), vec![DestinationKind::Dashboard]);

        // Connected + configured → those appear too.
        let rich = DeliveryCapabilities {
            connected_platforms: vec![Platform::Discord],
            enabled: vec![DestinationKind::PlatformChannel, DestinationKind::Email],
            configured: vec![DestinationKind::Email],
        };
        let kinds = visible_kinds(&rich);
        assert!(kinds.contains(&DestinationKind::Dashboard));
        assert!(kinds.contains(&DestinationKind::PlatformChannel));
        assert!(kinds.contains(&DestinationKind::Email));
    }
}

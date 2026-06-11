//! Delivery destinations + capability gating (PRD §4, §4.3; DEL-010/011, DEN-*;
//! ADR-126 plugin sinks).
//!
//! Pure policy for *where* a summary may be sent. The dashboard store is
//! **always-on**; every other destination is **capability-gated**: it must be
//! admin-enabled and configured for the workspace, and platform destinations
//! must resolve against an actual connection (DEL-010). Gating is enforced here,
//! server-side (DEL-011). The actual send is host I/O (a per-destination
//! deliverer); this only decides whether it's allowed.
//!
//! A destination's `kind` is an **open string** ("webhook", "email",
//! "confluence", "platform_channel", …) so optional sink plugins (ADR-126) add
//! kinds without editing this enum. Gating keys off the [`DeliveryClass`], not a
//! fixed kind list, so a new service sink is free here.

use crate::Platform;

/// How a destination is gated. Set by whoever builds the [`Destination`] (the
/// host knows whether a kind is a connected-platform target or a configured
/// service), so this module stays agnostic to the open set of kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryClass {
    /// Always-on dashboard store (PRD §4 item 3) — never gated.
    Dashboard,
    /// A connected-platform channel/DM — needs a `WorkspaceConnection` (DEL-010).
    Platform,
    /// A configured external service (email, webhook, confluence, …) — needs
    /// admin-enable + present config (DEN-*).
    Service,
}

/// A concrete destination instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// Open kind id ("webhook", "confluence", "platform_channel", …).
    pub kind: String,
    /// Gating class for this kind.
    pub class: DeliveryClass,
    /// Required for `Platform`-class destinations.
    pub platform: Option<Platform>,
}

impl Destination {
    /// A `Service`-class destination (email/webhook/plugin sink).
    pub fn service(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            class: DeliveryClass::Service,
            platform: None,
        }
    }
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
}

/// The gating decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryDecision {
    Allowed,
    Rejected(DeliveryReject),
}

/// What a workspace can deliver to: its connected platforms, which destination
/// kinds an admin has enabled, and which have their config present. Kinds are
/// the open string ids.
#[derive(Debug, Clone, Default)]
pub struct DeliveryCapabilities {
    pub connected_platforms: Vec<Platform>,
    pub enabled: Vec<String>,
    pub configured: Vec<String>,
}

impl DeliveryCapabilities {
    fn is_enabled(&self, kind: &str) -> bool {
        self.enabled.iter().any(|k| k == kind)
    }
    fn is_configured(&self, kind: &str) -> bool {
        self.configured.iter().any(|k| k == kind)
    }
    fn is_connected(&self, platform: Platform) -> bool {
        self.connected_platforms.contains(&platform)
    }
}

/// Decide whether `dest` may receive a summary given the workspace `caps`.
pub fn resolve(dest: &Destination, caps: &DeliveryCapabilities) -> DeliveryDecision {
    match dest.class {
        // The dashboard store is always available (PRD §4 item 3).
        DeliveryClass::Dashboard => DeliveryDecision::Allowed,
        // A connected-platform target (DEL-010).
        DeliveryClass::Platform => {
            if !caps.is_enabled(&dest.kind) {
                return DeliveryDecision::Rejected(DeliveryReject::NotEnabled);
            }
            let Some(platform) = dest.platform else {
                return DeliveryDecision::Rejected(DeliveryReject::PlatformMissing);
            };
            if caps.is_connected(platform) {
                DeliveryDecision::Allowed
            } else {
                DeliveryDecision::Rejected(DeliveryReject::PlatformNotConnected)
            }
        }
        // A configured external service / plugin sink (DEN-*).
        DeliveryClass::Service => {
            if !caps.is_enabled(&dest.kind) {
                DeliveryDecision::Rejected(DeliveryReject::NotEnabled)
            } else if !caps.is_configured(&dest.kind) {
                DeliveryDecision::Rejected(DeliveryReject::NotConfigured)
            } else {
                DeliveryDecision::Allowed
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn platform_dest(platform: Option<Platform>) -> Destination {
        Destination {
            kind: "platform_channel".into(),
            class: DeliveryClass::Platform,
            platform,
        }
    }

    #[test]
    fn dashboard_is_always_allowed_even_with_no_capabilities() {
        let caps = DeliveryCapabilities::default();
        let dash = Destination {
            kind: "dashboard".into(),
            class: DeliveryClass::Dashboard,
            platform: None,
        };
        assert_eq!(resolve(&dash, &caps), DeliveryDecision::Allowed);
    }

    #[test]
    fn platform_channel_requires_a_connection() {
        let caps = DeliveryCapabilities {
            connected_platforms: vec![Platform::Slack],
            enabled: vec!["platform_channel".into()],
            configured: vec![],
        };
        // Discord not connected → rejected (DEL-010).
        assert_eq!(
            resolve(&platform_dest(Some(Platform::Discord)), &caps),
            DeliveryDecision::Rejected(DeliveryReject::PlatformNotConnected)
        );
        // Slack connected → allowed.
        assert_eq!(
            resolve(&platform_dest(Some(Platform::Slack)), &caps),
            DeliveryDecision::Allowed
        );
    }

    #[test]
    fn disabled_service_is_rejected_server_side() {
        let caps = DeliveryCapabilities::default(); // nothing enabled
        assert_eq!(
            resolve(&Destination::service("email"), &caps),
            DeliveryDecision::Rejected(DeliveryReject::NotEnabled)
        );
    }

    #[test]
    fn enabled_but_unconfigured_service_is_rejected() {
        let caps = DeliveryCapabilities {
            enabled: vec!["webhook".into()],
            configured: vec![], // no config
            ..DeliveryCapabilities::default()
        };
        assert_eq!(
            resolve(&Destination::service("webhook"), &caps),
            DeliveryDecision::Rejected(DeliveryReject::NotConfigured)
        );
    }

    #[test]
    fn enabled_and_configured_service_is_allowed() {
        let caps = DeliveryCapabilities {
            enabled: vec!["confluence".into()],
            configured: vec!["confluence".into()],
            ..DeliveryCapabilities::default()
        };
        // An open plugin kind the policy has never heard of is gated uniformly.
        assert_eq!(
            resolve(&Destination::service("confluence"), &caps),
            DeliveryDecision::Allowed
        );
    }
}

//! Host-header → tenant routing (TEN-006), pure.
//!
//! Multi-tenancy is reached two ways (PRD §6): a per-tenant **subdomain** under
//! the product's base domain (`acme.summarybot.app`, TEN-001) and a tenant's own
//! **custom domain** (`chat.acme.com`, TEN-002). This module decides, from an
//! incoming `Host` header and the configured base domain, *which* of those a
//! request is using — [`route_host`] returns the routing shape, and the host
//! layer resolves it against storage. No I/O, no clock: just string policy.

/// How an incoming host maps to tenant routing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRoute {
    /// The base domain itself or its `www.` — the marketing/apex surface, not a
    /// tenant.
    Apex,
    /// A single-label subdomain under the base domain (`acme` in
    /// `acme.summarybot.app`) — resolved to a tenant by subdomain (TEN-001).
    Subdomain(String),
    /// A host that is not under the base domain — a tenant's own custom domain
    /// (TEN-002), resolved by exact match.
    CustomDomain(String),
}

/// Normalize a `Host` header for comparison: trim, lowercase, drop any `:port`
/// and a trailing dot. Returns `None` for an empty/invalid host.
fn normalize_host(host: &str) -> Option<String> {
    let host = host.trim();
    // Strip a port suffix. IPv6 literals (`[::1]:8080`) aren't valid tenant
    // hosts, so a leading '[' is rejected outright below.
    let host = host.split(':').next().unwrap_or(host);
    let host = host.strip_suffix('.').unwrap_or(host);
    let host = host.to_ascii_lowercase();
    if host.is_empty() || host.starts_with('[') || host.starts_with('.') {
        return None;
    }
    Some(host)
}

/// Decide how `host` routes given the product's `base_domain` (e.g.
/// `summarybot.app`). The `base_domain` is matched case-insensitively.
///
/// - `summarybot.app` / `www.summarybot.app` → [`HostRoute::Apex`]
/// - `acme.summarybot.app` → [`HostRoute::Subdomain`]`("acme")`
/// - `a.b.summarybot.app` (multi-label) → treated as a custom domain: only a
///   single label is a tenant subdomain, so deeper hosts don't silently alias.
/// - anything not under the base domain → [`HostRoute::CustomDomain`]
///
/// Returns `None` if either the host or base domain is empty/invalid.
pub fn route_host(host: &str, base_domain: &str) -> Option<HostRoute> {
    let host = normalize_host(host)?;
    let base = normalize_host(base_domain)?;

    if host == base || host == format!("www.{base}") {
        return Some(HostRoute::Apex);
    }
    if let Some(prefix) = host.strip_suffix(&format!(".{base}")) {
        // Exactly one label below the base is a tenant subdomain; an empty or
        // multi-label prefix is not (it can't be a single tenant key).
        if !prefix.is_empty() && !prefix.contains('.') {
            return Some(HostRoute::Subdomain(prefix.to_string()));
        }
        return Some(HostRoute::CustomDomain(host));
    }
    Some(HostRoute::CustomDomain(host))
}

/// Validate + normalize a tenant subdomain label (TEN-001): 1–63 chars of ASCII
/// lowercase letters, digits, and hyphens, with no leading/trailing hyphen.
/// `www` is reserved (it routes to the apex, [`HostRoute::Apex`]). Returns the
/// normalized (trimmed, lowercased) label, or `None` if invalid.
pub fn normalize_subdomain(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s.len() > 63 || s == "www" {
        return None;
    }
    if s.starts_with('-') || s.ends_with('-') {
        return None;
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return None;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "summarybot.app";

    #[test]
    fn apex_and_www_are_apex() {
        assert_eq!(route_host("summarybot.app", BASE), Some(HostRoute::Apex));
        assert_eq!(
            route_host("www.summarybot.app", BASE),
            Some(HostRoute::Apex)
        );
    }

    #[test]
    fn single_label_subdomain() {
        assert_eq!(
            route_host("acme.summarybot.app", BASE),
            Some(HostRoute::Subdomain("acme".into()))
        );
    }

    #[test]
    fn port_and_case_and_trailing_dot_are_normalized() {
        assert_eq!(
            route_host("ACME.SummaryBot.app:8080", BASE),
            Some(HostRoute::Subdomain("acme".into()))
        );
        assert_eq!(
            route_host("acme.summarybot.app.", BASE),
            Some(HostRoute::Subdomain("acme".into()))
        );
    }

    #[test]
    fn unrelated_host_is_a_custom_domain() {
        assert_eq!(
            route_host("chat.acme.com", BASE),
            Some(HostRoute::CustomDomain("chat.acme.com".into()))
        );
    }

    #[test]
    fn multi_label_under_base_is_not_a_subdomain() {
        // a.b.summarybot.app can't be one tenant key — treat as custom domain
        // so it never silently aliases onto subdomain "a".
        assert_eq!(
            route_host("a.b.summarybot.app", BASE),
            Some(HostRoute::CustomDomain("a.b.summarybot.app".into()))
        );
    }

    #[test]
    fn empty_or_invalid_host_is_none() {
        assert_eq!(route_host("", BASE), None);
        assert_eq!(route_host("   ", BASE), None);
        assert_eq!(route_host("[::1]:8080", BASE), None);
        assert_eq!(route_host("acme.summarybot.app", ""), None);
    }

    #[test]
    fn subdomain_validation() {
        assert_eq!(normalize_subdomain("Acme"), Some("acme".into()));
        assert_eq!(normalize_subdomain("  team-1 "), Some("team-1".into()));
        assert_eq!(normalize_subdomain("a"), Some("a".into()));
        // Reserved / invalid.
        assert_eq!(normalize_subdomain("www"), None);
        assert_eq!(normalize_subdomain(""), None);
        assert_eq!(normalize_subdomain("-lead"), None);
        assert_eq!(normalize_subdomain("trail-"), None);
        assert_eq!(normalize_subdomain("has space"), None);
        assert_eq!(normalize_subdomain("under_score"), None);
        assert_eq!(normalize_subdomain(&"x".repeat(64)), None);
    }

    #[test]
    fn a_host_that_merely_ends_with_base_text_is_custom() {
        // "notsummarybot.app" ends with "summarybot.app" textually but is not
        // *under* it (no dot boundary) → custom domain, not apex/subdomain.
        assert_eq!(
            route_host("notsummarybot.app", BASE),
            Some(HostRoute::CustomDomain("notsummarybot.app".into()))
        );
    }
}

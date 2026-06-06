//! Host-side tenant resolution (TEN-006) — the seam that turns the pure
//! [`route_host`] decision into a tenant lookup.
//!
//! The domain decides *how* a `Host` header routes (apex / subdomain / custom
//! domain); this resolves that decision against the repository. The apex
//! surface has no tenant, so it returns `Ok(None)` — distinct from "a tenant
//! host that matched nothing", which also returns `Ok(None)` but is the
//! caller's cue to 404. Both are non-errors; only storage failures are `Err`.

use anyhow::Result;
use domain::{route_host, HostRoute, Tenant};
use repository::WorkspaceRepository;

/// Resolve the tenant for an incoming `host` header given the product's
/// `base_domain`. Returns the matched [`Tenant`], or `None` when the host is the
/// apex/marketing surface, is malformed, or names no known tenant.
pub fn resolve_tenant_by_host(
    repo: &impl WorkspaceRepository,
    host: &str,
    base_domain: &str,
) -> Result<Option<Tenant>> {
    match route_host(host, base_domain) {
        // Apex / www / malformed host: no tenant context.
        None | Some(HostRoute::Apex) => Ok(None),
        Some(HostRoute::Subdomain(sub)) => repo.find_tenant_by_subdomain(&sub),
        Some(HostRoute::CustomDomain(domain)) => repo.find_tenant_by_custom_domain(&domain),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::TenantId;
    use repository::SqliteRepository;

    const BASE: &str = "summarybot.app";

    fn repo_with_acme() -> SqliteRepository {
        let repo = SqliteRepository::in_memory().unwrap();
        repo.create_tenant(
            &Tenant::new(
                TenantId::parse("t-acme").unwrap(),
                "Acme",
                Some("acme".into()),
                Some("chat.acme.com".into()),
            )
            .unwrap(),
        )
        .unwrap();
        repo
    }

    #[test]
    fn subdomain_resolves_to_its_tenant() {
        let repo = repo_with_acme();
        let t = resolve_tenant_by_host(&repo, "acme.summarybot.app", BASE)
            .unwrap()
            .unwrap();
        assert_eq!(t.id.as_str(), "t-acme");
    }

    #[test]
    fn custom_domain_resolves_to_its_tenant() {
        let repo = repo_with_acme();
        let t = resolve_tenant_by_host(&repo, "chat.acme.com:443", BASE)
            .unwrap()
            .unwrap();
        assert_eq!(t.id.as_str(), "t-acme");
    }

    #[test]
    fn apex_has_no_tenant() {
        let repo = repo_with_acme();
        assert!(resolve_tenant_by_host(&repo, "www.summarybot.app", BASE)
            .unwrap()
            .is_none());
    }

    #[test]
    fn unknown_subdomain_is_none() {
        let repo = repo_with_acme();
        assert!(resolve_tenant_by_host(&repo, "ghost.summarybot.app", BASE)
            .unwrap()
            .is_none());
    }
}

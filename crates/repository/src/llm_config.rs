//! Per-tenant LLM provider config (ADR-125 Phase 2a).
//!
//! A tenant may point summarization at its own OpenAI-compatible endpoint and/or
//! pin a model — "bring your own LLM" for the self-hosted/local case. Keyless
//! for now; the encrypted BYO-key column is Phase 2b (gated on the
//! encryption-at-rest decision). Tenant-scoped like everything else (TEN-007).

use crate::SqliteRepository;
use anyhow::Result;
use domain::TenantId;
use rusqlite::{params, OptionalExtension};

/// A tenant's LLM config. Both fields optional: a tenant may set only an
/// endpoint, only a model, or both. An all-`None` config is equivalent to none.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TenantLlmConfig {
    /// OpenAI-compatible base URL (e.g. the tenant's own Ollama/vLLM); `None`
    /// uses the process-default backend.
    pub base_url: Option<String>,
    /// Model name to request; `None` uses the process-default model.
    pub model: Option<String>,
    /// Encrypted BYO API key (ADR-125 Phase 2b) — opaque ciphertext as produced
    /// by the host's secretbox; the repository never sees the plaintext. `None`
    /// for keyless (self-hosted) configs.
    pub api_key_enc: Option<String>,
}

/// Storage boundary for per-tenant LLM config.
pub trait LlmConfigRepository {
    /// Upsert a tenant's config (replaces any prior values).
    fn set_llm_config(&self, tenant: &TenantId, config: &TenantLlmConfig) -> Result<()>;
    /// Fetch a tenant's config, or `None` if unset.
    fn get_llm_config(&self, tenant: &TenantId) -> Result<Option<TenantLlmConfig>>;
    /// Remove a tenant's config (revert to process defaults). Returns whether a
    /// row was removed.
    fn clear_llm_config(&self, tenant: &TenantId) -> Result<bool>;
}

impl LlmConfigRepository for SqliteRepository {
    fn set_llm_config(&self, tenant: &TenantId, config: &TenantLlmConfig) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tenant_llm_config (tenant_id, base_url, model, api_key_enc)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(tenant_id) DO UPDATE SET base_url = excluded.base_url,
                                                  model = excluded.model,
                                                  api_key_enc = excluded.api_key_enc",
            params![
                tenant.as_str(),
                config.base_url,
                config.model,
                config.api_key_enc
            ],
        )?;
        Ok(())
    }

    fn get_llm_config(&self, tenant: &TenantId) -> Result<Option<TenantLlmConfig>> {
        self.conn
            .query_row(
                "SELECT base_url, model, api_key_enc FROM tenant_llm_config WHERE tenant_id = ?1",
                params![tenant.as_str()],
                |row| {
                    Ok(TenantLlmConfig {
                        base_url: row.get(0)?,
                        model: row.get(1)?,
                        api_key_enc: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    fn clear_llm_config(&self, tenant: &TenantId) -> Result<bool> {
        let n = self.conn.execute(
            "DELETE FROM tenant_llm_config WHERE tenant_id = ?1",
            params![tenant.as_str()],
        )?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        SqliteRepository::in_memory().unwrap()
    }
    fn t(id: &str) -> TenantId {
        TenantId::parse(id).unwrap()
    }

    #[test]
    fn set_get_update_clear_round_trip() {
        let repo = repo();
        assert!(repo.get_llm_config(&t("t1")).unwrap().is_none());

        repo.set_llm_config(
            &t("t1"),
            &TenantLlmConfig {
                base_url: Some("http://mac-mini:11434/v1".into()),
                model: Some("llama3.1".into()),
                api_key_enc: Some("ciphertext-blob".into()),
            },
        )
        .unwrap();
        let got = repo.get_llm_config(&t("t1")).unwrap().unwrap();
        assert_eq!(got.base_url.as_deref(), Some("http://mac-mini:11434/v1"));
        assert_eq!(got.model.as_deref(), Some("llama3.1"));
        assert_eq!(got.api_key_enc.as_deref(), Some("ciphertext-blob"));

        // Upsert replaces (and can clear the key).
        repo.set_llm_config(
            &t("t1"),
            &TenantLlmConfig {
                base_url: None,
                model: Some("qwen2.5".into()),
                api_key_enc: None,
            },
        )
        .unwrap();
        let got = repo.get_llm_config(&t("t1")).unwrap().unwrap();
        assert!(got.base_url.is_none());
        assert_eq!(got.model.as_deref(), Some("qwen2.5"));
        assert!(got.api_key_enc.is_none());

        // Scoped per tenant.
        assert!(repo.get_llm_config(&t("t2")).unwrap().is_none());

        assert!(repo.clear_llm_config(&t("t1")).unwrap());
        assert!(repo.get_llm_config(&t("t1")).unwrap().is_none());
        assert!(!repo.clear_llm_config(&t("t1")).unwrap());
    }
}

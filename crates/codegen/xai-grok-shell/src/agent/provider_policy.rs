//! Fork provider-policy layer on top of upstream `[model_providers.<id>]`.
//!
//! Upstream owns the provider table, its parsing
//! (`model_providers::parse_model_providers`), and its inheritance
//! (`ConfigModelOverride::with_provider_defaults`): the provider supplies
//! defaults, per-model values win. This module keeps the fork's provider
//! semantics out of that path and adds them as a side layer:
//!
//! - [`normalize_provider_config`] folds the legacy `[provider.<name>]` tables
//!   and the legacy `provider = "<id>"` model binding into the upstream shapes
//!   before parsing, so both spellings resolve through the single upstream
//!   inheritance path.
//! - [`split_provider_policies`] lifts the fork-only keys (`auth_scheme`,
//!   `max_retries`, `inference_idle_timeout_secs`, `prompt_cache`) out of the
//!   raw provider tables into [`ProviderPolicy`], so upstream parsing never
//!   reports them as unknown fields.
//! - [`apply_provider_policy`] layers the policy onto a resolved
//!   [`ModelEntry`] after upstream `apply()`: model-level values win, and an
//!   explicit `auth_scheme` marks the model provider-bound, which opts it out
//!   of ambient xAI credentials
//!   (`ModelEntry::opts_out_of_ambient_credentials`).

use indexmap::IndexMap;
use serde::Deserialize;

use super::config::{ModelEntry, ResolvedProviderBinding};
use super::config_model_override_parse::{ConfigWarning, ConfigWarningKind};
use xai_grok_sampler::AuthScheme;
use xai_grok_sampling_types::PromptCachePolicy;

/// Authentication header policy for a provider with an explicit `auth_scheme`.
///
/// Mirrors the legacy `[provider.<name>].auth` values. When set, the provider
/// owns the model's authentication boundary: provider-bound models never fall
/// back to the ambient xAI session token or `XAI_API_KEY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAuth {
    /// Send the configured provider credential as `Authorization: Bearer`.
    Bearer,
    /// Send the configured provider credential as `x-api-key`.
    XApiKey,
    /// Send no authentication header.
    None,
}

impl ProviderAuth {
    /// The request header this scheme owns. `extra_headers` entries with this
    /// name (case-insensitive) would bypass the scheme and are rejected.
    pub fn protected_header(self) -> &'static str {
        match self {
            ProviderAuth::XApiKey => "x-api-key",
            ProviderAuth::Bearer | ProviderAuth::None => "authorization",
        }
    }
}

/// Fork-only provider fields, split out of `[model_providers.<id>]` tables.
///
/// A policy with `auth: None` only contributes sampling defaults (TTL,
/// retry, timeout) and does not change the model's credential behavior.
#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct ProviderPolicy {
    /// Explicit authentication scheme; presence marks a credential boundary.
    pub auth: Option<ProviderAuth>,
    pub max_retries: Option<u32>,
    pub inference_idle_timeout_secs: Option<u64>,
    /// Prompt-cache policy inherited by bound models that set none of their own.
    pub prompt_cache: PromptCachePolicy,
}

impl ProviderPolicy {
    fn is_empty(&self) -> bool {
        self.auth.is_none()
            && self.max_retries.is_none()
            && self.inference_idle_timeout_secs.is_none()
            && self.prompt_cache.is_default()
    }
}

/// Fold the legacy provider spellings into the upstream ones.
///
/// - `[model.<id>] provider = "<id>"` becomes `model_provider = "<id>"`.
/// - `[provider.<name>]` becomes `[model_providers.<name>]` with `auth`
///   renamed to `auth_scheme`. A legacy table always carries an explicit auth
///   scheme (defaulting to `bearer`) because the legacy provider owned
///   credentials unconditionally.
///
/// Setting both spellings is an error rather than a precedence rule, the same
/// way serde aliases reject a table containing both keys.
pub(crate) fn normalize_provider_config(mut raw: toml::Value) -> Result<toml::Value, String> {
    if let Some(models) = raw.get_mut("model").and_then(|m| m.as_table_mut()) {
        for (model_id, value) in models.iter_mut() {
            let Some(table) = value.as_table_mut() else {
                continue;
            };
            if let Some(binding) = table.remove("provider") {
                if table.contains_key("model_provider") {
                    return Err(format!(
                        "model.{model_id} sets both `provider` and `model_provider`; \
                         use `model_provider`"
                    ));
                }
                table.insert("model_provider".to_owned(), binding);
            }
        }
    }
    let Some(root) = raw.as_table_mut() else {
        return Ok(raw);
    };
    let Some(provider_section) = root.remove("provider") else {
        return Ok(raw);
    };
    let legacy = match provider_section {
        toml::Value::Table(table) => table,
        other => {
            return Err(format!(
                "`provider` must be a table of [provider.<name>] entries, got {}",
                other.type_str()
            ));
        }
    };
    let mut merged = match root.remove("model_providers") {
        None => toml::map::Map::new(),
        Some(toml::Value::Table(table)) => table,
        Some(other) => {
            return Err(format!(
                "`model_providers` must be a table of [model_providers.<id>] entries, got {}",
                other.type_str()
            ));
        }
    };
    for (name, mut value) in legacy {
        let Some(table) = value.as_table_mut() else {
            return Err(format!(
                "provider.{name} must be a table like [provider.{name}], got {}",
                value.type_str()
            ));
        };
        if let Some(auth) = table.remove("auth") {
            if table.contains_key("auth_scheme") {
                return Err(format!(
                    "provider.{name} sets both `auth` and `auth_scheme`; use `auth_scheme`"
                ));
            }
            table.insert("auth_scheme".to_owned(), auth);
        }
        table
            .entry("auth_scheme".to_owned())
            .or_insert_with(|| toml::Value::String("bearer".to_owned()));
        if merged.contains_key(&name) {
            return Err(format!(
                "provider.{name} is also declared as [model_providers.{name}]; \
                 declare the provider once"
            ));
        }
        merged.insert(name, value);
    }
    root.insert("model_providers".to_owned(), toml::Value::Table(merged));
    Ok(raw)
}

/// Lift the fork-only keys out of the raw `[model_providers.<id>]` tables.
///
/// Returns the raw config with those keys removed (so upstream
/// `parse_model_providers` never sees unknown fields) alongside the parsed
/// policies. Per-field parse failures become config warnings and the offending
/// key is ignored, matching the lenient parsing style of the surrounding
/// provider tables.
pub(crate) fn split_provider_policies(
    raw: &toml::Value,
) -> (
    toml::Value,
    IndexMap<String, ProviderPolicy>,
    Vec<ConfigWarning>,
) {
    let mut stripped = raw.clone();
    let mut policies = IndexMap::new();
    let mut warnings = Vec::new();
    let Some(section) = stripped
        .get_mut("model_providers")
        .and_then(|value| value.as_table_mut())
    else {
        return (stripped, policies, warnings);
    };
    for (id, value) in section.iter_mut() {
        let Some(table) = value.as_table_mut() else {
            continue;
        };
        let mut policy = ProviderPolicy::default();
        let mut changed = false;
        if let Some(auth_value) = table.remove("auth_scheme") {
            changed = true;
            match ProviderAuth::deserialize(auth_value) {
                Ok(auth) => policy.auth = Some(auth),
                Err(error) => warnings.push(ConfigWarning::model_provider(
                    id,
                    Some("auth_scheme"),
                    ConfigWarningKind::InvalidValue,
                    format!("invalid auth_scheme ({error}); expected bearer, x_api_key, or none"),
                )),
            }
        }
        if let Some(retries_value) = table.remove("max_retries") {
            changed = true;
            match retries_value.as_integer() {
                Some(retries) if (0..=i64::from(u32::MAX)).contains(&retries) => {
                    policy.max_retries = Some(retries as u32);
                }
                _ => warnings.push(ConfigWarning::model_provider(
                    id,
                    Some("max_retries"),
                    ConfigWarningKind::InvalidValue,
                    "expected a non-negative integer; ignored".to_owned(),
                )),
            }
        }
        if let Some(timeout_value) = table.remove("inference_idle_timeout_secs") {
            changed = true;
            match timeout_value.as_integer() {
                // Any non-negative i64 fits in u64; `u64::MAX as i64` would wrap.
                Some(timeout) if timeout >= 0 => {
                    policy.inference_idle_timeout_secs = Some(timeout as u64);
                }
                _ => warnings.push(ConfigWarning::model_provider(
                    id,
                    Some("inference_idle_timeout_secs"),
                    ConfigWarningKind::InvalidValue,
                    "expected a non-negative integer; ignored".to_owned(),
                )),
            }
        }
        if let Some(cache_value) = table.remove("prompt_cache") {
            changed = true;
            match PromptCachePolicy::deserialize(cache_value) {
                Ok(policy_cache) => policy.prompt_cache = policy_cache,
                Err(error) => warnings.push(ConfigWarning::model_provider(
                    id,
                    Some("prompt_cache"),
                    ConfigWarningKind::InvalidValue,
                    format!("invalid prompt_cache ({error}); ignored"),
                )),
            }
        }
        if changed && !policy.is_empty() {
            policies.insert(id.clone(), policy);
        }
    }
    (stripped, policies, warnings)
}

/// Layer a provider policy onto a resolved entry, after upstream
/// `ConfigModelOverride::apply` and the fail-closed auth-provider guard.
///
/// Model-level values already on the entry win; the policy only fills what
/// the model left unset. An explicit `auth_scheme` marks the entry
/// provider-bound: `auth = "none"` strips credentials (unauthenticated
/// endpoint), anything else requires one and both refuse the ambient xAI
/// credential at resolution time.
pub(crate) fn apply_provider_policy(
    entry: &mut ModelEntry,
    provider_id: &str,
    policy: &ProviderPolicy,
) {
    if entry.info.prompt_cache.is_default() {
        entry.info.prompt_cache = policy.prompt_cache;
    }
    if entry.info.max_retries.is_none() {
        entry.info.max_retries = policy.max_retries;
    }
    if entry.info.inference_idle_timeout_secs.is_none() {
        entry.info.inference_idle_timeout_secs = policy.inference_idle_timeout_secs;
    }
    let Some(auth) = policy.auth else {
        return;
    };
    entry.info.auth_scheme = match auth {
        ProviderAuth::XApiKey => AuthScheme::XApiKey,
        ProviderAuth::Bearer | ProviderAuth::None => AuthScheme::Bearer,
    };
    let auth_required = auth != ProviderAuth::None;
    if auth_required {
        // The upstream fail-closed guard may have stamped a synthetic
        // auth-provider ref because the session bearer is unsafe on this URL.
        // A strict binding already refuses ambient credentials at resolution
        // time, and the synthetic ref would make preflight treat the model as
        // holding a credential it can never mint, so drop it.
        if entry
            .auth_provider
            .as_ref()
            .is_some_and(xai_grok_login::AuthProviderRef::is_fail_closed)
        {
            entry.auth_provider = None;
        }
    } else {
        entry.api_key = None;
        entry.env_key = None;
        entry.auth_provider = None;
    }
    entry.provider = Some(ResolvedProviderBinding {
        id: provider_id.to_owned(),
        auth_required,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::config::{Config, resolve_credentials, resolve_model_list};
    use xai_grok_sampler::AuthScheme;

    fn config_from(toml_src: &str) -> Config {
        let raw: toml::Value = toml::from_str(toml_src).expect("valid test TOML");
        Config::new_from_toml_cfg(&raw).expect("config should parse")
    }

    #[test]
    fn legacy_provider_alias_resolves_through_upstream_inheritance() {
        let legacy = config_from(
            r#"
            [provider.anthropic]
            base_url = "https://api.anthropic.com/v1"
            api_backend = "messages"
            auth = "x_api_key"
            env_key = "ANTHROPIC_API_KEY"

            [model.via-legacy]
            provider = "anthropic"
            model = "claude-sonnet"
            context_window = 200000
            "#,
        );
        let unified = config_from(
            r#"
            [model_providers.anthropic]
            base_url = "https://api.anthropic.com/v1"
            api_backend = "messages"
            auth_scheme = "x_api_key"
            env_key = "ANTHROPIC_API_KEY"

            [model.via-unified]
            model_provider = "anthropic"
            model = "claude-sonnet"
            context_window = 200000
            "#,
        );
        let legacy_entry = resolve_model_list(&legacy, None)
            .get("via-legacy")
            .expect("legacy-bound model should exist")
            .clone();
        let unified_entry = resolve_model_list(&unified, None)
            .get("via-unified")
            .expect("unified-bound model should exist")
            .clone();
        assert_eq!(legacy_entry.info.base_url, unified_entry.info.base_url);
        assert_eq!(legacy_entry.env_key, unified_entry.env_key);
        assert_eq!(legacy_entry.info.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(
            legacy_entry.provider, unified_entry.provider,
            "both spellings carry the same credential-boundary marker"
        );
        let binding = legacy_entry.provider.as_ref().expect("binding marker");
        assert_eq!(binding.id, "anthropic");
        assert!(binding.auth_required);
        assert_eq!(
            resolve_credentials(&legacy_entry, Some("session-jwt")).api_key,
            None,
            "a provider-bound model never borrows the ambient session token"
        );
    }

    #[test]
    fn unified_syntax_carries_fork_policy() {
        let cfg = config_from(
            r#"
            [model_providers.anthropic]
            base_url = "https://api.anthropic.com/v1"
            api_backend = "messages"
            auth_scheme = "x_api_key"
            api_key = "sk-ant"
            max_retries = 5
            inference_idle_timeout_secs = 300
            prompt_cache = { mode = "stable_prefix", ttl = "1h" }

            [model.claude]
            model_provider = "anthropic"
            model = "claude-sonnet"
            "#,
        );
        let resolved = resolve_model_list(&cfg, None);
        let entry = resolved.get("claude").expect("model should exist");
        assert_eq!(entry.info.auth_scheme, AuthScheme::XApiKey);
        assert_eq!(entry.info.max_retries, Some(5));
        assert_eq!(entry.info.inference_idle_timeout_secs, Some(300));
        assert_eq!(
            entry.info.prompt_cache.ttl,
            xai_grok_sampling_types::PromptCacheTtl::OneHour
        );
        assert!(entry.provider.as_ref().is_some_and(|b| b.auth_required));
    }

    #[test]
    fn model_level_prompt_cache_wins_over_provider_policy() {
        let cfg = config_from(
            r#"
            [model_providers.gateway]
            base_url = "https://gateway.example/v1"
            api_key = "k"
            auth_scheme = "bearer"
            prompt_cache = { mode = "stable_prefix", ttl = "1h" }

            [model.off]
            model_provider = "gateway"
            model = "m"
            prompt_cache = { mode = "off" }
            "#,
        );
        let resolved = resolve_model_list(&cfg, None);
        let entry = resolved.get("off").expect("model");
        assert_eq!(
            entry.info.prompt_cache,
            xai_grok_sampling_types::PromptCachePolicy::OFF
        );
    }

    #[test]
    fn policy_without_auth_scheme_keeps_upstream_semantics() {
        let cfg = config_from(
            r#"
            [model_providers.shared]
            prompt_cache = { mode = "stable_prefix", ttl = "1h" }
            extra_headers = { X-Corp = "yes" }

            [model.via-shared]
            model_provider = "shared"
            model = "m"
            "#,
        );
        let resolved = resolve_model_list(&cfg, None);
        let entry = resolved.get("via-shared").expect("model should exist");
        assert_eq!(
            entry.info.prompt_cache.ttl,
            xai_grok_sampling_types::PromptCacheTtl::OneHour
        );
        assert!(
            entry.provider.is_none(),
            "a provider that shares no credentials is not a trust boundary; \
             upstream ambient semantics apply"
        );
    }

    #[test]
    fn auth_scheme_none_strips_credentials() {
        let cfg = config_from(
            r#"
            [model_providers.local]
            base_url = "http://localhost:8080/v1"
            auth_scheme = "none"
            api_key = "ignored-by-policy"

            [model.local]
            model_provider = "local"
            model = "m"
            "#,
        );
        let resolved = resolve_model_list(&cfg, None);
        let entry = resolved.get("local").expect("model");
        assert!(entry.api_key.is_none());
        assert!(entry.env_key.is_none());
        assert!(entry.auth_provider.is_none());
        let binding = entry.provider.as_ref().expect("binding marker");
        assert!(!binding.auth_required);
        assert!(
            entry.route_preflight_ready(),
            "an unauthenticated provider is route-eligible without credentials"
        );
    }

    #[test]
    fn both_binding_spellings_are_rejected() {
        let raw: toml::Value = toml::from_str(
            r#"
            [model_providers.gateway]
            base_url = "https://gateway.example/v1"

            [model.both]
            provider = "gateway"
            model_provider = "gateway"
            model = "m"
            "#,
        )
        .unwrap();
        let err = Config::new_from_toml_cfg(&raw).expect_err("both spellings must be rejected");
        assert!(
            err.contains("both `provider` and `model_provider`"),
            "{err}"
        );
    }

    #[test]
    fn duplicate_provider_declaration_is_rejected() {
        let raw: toml::Value = toml::from_str(
            r#"
            [provider.gateway]
            base_url = "https://legacy.example/v1"

            [model_providers.gateway]
            base_url = "https://unified.example/v1"
            "#,
        )
        .unwrap();
        let err = Config::new_from_toml_cfg(&raw).expect_err("duplicate declaration");
        assert!(
            err.contains("also declared as [model_providers.gateway]"),
            "{err}"
        );
    }

    #[test]
    fn strict_providers_require_base_url_and_credentials() {
        let missing_creds = config_from(
            r#"
            [model_providers.strict]
            base_url = "https://strict.example/v1"
            auth_scheme = "bearer"

            [model.via-strict]
            model_provider = "strict"
            model = "m"
            "#,
        );
        let err = missing_creds
            .validate_model_filters()
            .expect_err("bearer scheme requires a credential");
        assert!(
            err.contains("requires api_key, env_key, or auth_provider"),
            "{err}"
        );

        let none_with_creds = config_from(
            r#"
            [model_providers.anon]
            base_url = "http://localhost:8080/v1"
            auth_scheme = "none"
            api_key = "k"
            "#,
        );
        let err = none_with_creds
            .validate_model_filters()
            .expect_err("auth none rejects credentials");
        assert!(
            err.contains("uses auth_scheme = \"none\" but also configures a credential"),
            "{err}"
        );
    }

    #[test]
    fn strict_scheme_protects_its_authentication_header() {
        let provider_header = config_from(
            r#"
            [model_providers.anthropic]
            base_url = "https://api.anthropic.com/v1"
            auth_scheme = "x_api_key"
            env_key = "ANTHROPIC_API_KEY"
            extra_headers = { "x-api-key" = "bypass" }
            "#,
        );
        let err = provider_header
            .validate_model_filters()
            .expect_err("provider must not bypass its scheme");
        assert!(err.contains("must not set authentication header"), "{err}");

        let model_header = config_from(
            r#"
            [model_providers.anthropic]
            base_url = "https://api.anthropic.com/v1"
            auth_scheme = "x_api_key"
            env_key = "ANTHROPIC_API_KEY"

            [model.via-anthropic]
            model_provider = "anthropic"
            model = "claude"
            extra_headers = { "x-api-key" = "bypass" }
            "#,
        );
        let err = model_header
            .validate_model_filters()
            .expect_err("model must not bypass the provider scheme");
        assert!(
            err.contains("must not override provider authentication header"),
            "{err}"
        );
    }

    #[test]
    fn fork_provider_fields_do_not_surface_as_unknown_fields() {
        let cfg = config_from(
            r#"
            [model_providers.gateway]
            base_url = "https://gateway.example/v1"
            auth_scheme = "bearer"
            api_key = "k"
            max_retries = 4
            inference_idle_timeout_secs = 120
            prompt_cache = { mode = "stable_prefix", ttl = "1h" }

            [model.via-gateway]
            model_provider = "gateway"
            model = "m"
            "#,
        );
        assert!(
            !cfg.config_warnings.iter().any(|warning| matches!(
                warning.kind,
                super::super::config_model_override_parse::ConfigWarningKind::UnknownField
            ) && format!("{warning:?}")
                .contains("model_providers")),
            "fork fields are split before upstream parsing: {:#?}",
            cfg.config_warnings
        );
    }
}

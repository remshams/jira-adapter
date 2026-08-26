use std::env;

use thiserror::Error;
use url::{Host, Url};

pub const BASE_URL_VAR: &str = "JIRA_BASE_URL";
pub const EMAIL_VAR: &str = "JIRA_EMAIL";
pub const API_TOKEN_VAR: &str = "JIRA_API_TOKEN";
const ALLOW_INSECURE_HTTP_VAR: &str = "JCLI_ALLOW_INSECURE_HTTP";

/// Everything jcli needs to authenticate against a Jira Cloud instance:
/// the instance base URL plus an email and API token for Basic auth.
#[derive(Debug)]
pub struct Config {
    pub base_url: Url,
    pub email: String,
    pub api_token: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing environment variable {0}")]
    Missing(&'static str),
    #[error("environment variable {0} is set but empty")]
    Empty(&'static str),
    #[error("invalid URL in {0}: expected an HTTPS URL without embedded credentials")]
    InvalidBaseUrl(&'static str),
}

impl Config {
    /// Reads the three `JIRA_*` variables from the process environment.
    pub fn load() -> Result<Self, ConfigError> {
        Self::from_vars(|name| env::var(name).ok())
    }

    /// Testable core of `load`, taking variable lookup as a function so
    /// tests never touch the real process environment.
    fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let base_url = required(&get, BASE_URL_VAR)?;
        let allow_insecure_loopback = get(ALLOW_INSECURE_HTTP_VAR).as_deref() == Some("1");
        let base_url = normalize_base_url(&base_url, allow_insecure_loopback)?;
        Ok(Config {
            base_url,
            email: required(&get, EMAIL_VAR)?,
            api_token: required(&get, API_TOKEN_VAR)?,
        })
    }
}

fn required(
    get: impl Fn(&str) -> Option<String>,
    name: &'static str,
) -> Result<String, ConfigError> {
    match get(name) {
        None => Err(ConfigError::Missing(name)),
        Some(value) if value.trim().is_empty() => Err(ConfigError::Empty(name)),
        Some(value) => Ok(value),
    }
}

/// Ensures the base URL ends in `/` so that `Url::join` with a relative API
/// path appends to the path instead of replacing its last segment:
/// `https://x/browse` + `rest/...` would otherwise lose `/browse`.
fn normalize_base_url(raw: &str, allow_insecure_loopback: bool) -> Result<Url, ConfigError> {
    let mut url = Url::parse(raw).map_err(|_| ConfigError::InvalidBaseUrl(BASE_URL_VAR))?;
    let secure_transport = url.scheme() == "https";
    let allowed_test_transport = url.scheme() == "http"
        && allow_insecure_loopback
        && url.host().is_some_and(is_loopback_host);
    if (!secure_transport && !allowed_test_transport)
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ConfigError::InvalidBaseUrl(BASE_URL_VAR));
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

fn is_loopback_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    const ALL: &[(&str, &str)] = &[
        (BASE_URL_VAR, "https://example.atlassian.net"),
        (EMAIL_VAR, "jane@example.com"),
        (API_TOKEN_VAR, "secret-token"),
    ];

    #[test]
    fn loads_all_three_variables() {
        let config = Config::from_vars(vars(ALL)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.atlassian.net/");
        assert_eq!(config.email, "jane@example.com");
        assert_eq!(config.api_token, "secret-token");
    }

    #[test]
    fn keeps_a_base_url_with_a_path_prefix_intact() {
        let mut all = ALL.to_vec();
        all[0] = (BASE_URL_VAR, "https://example.com/jira");
        let config = Config::from_vars(vars(&all)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.com/jira/");
    }

    #[test]
    fn reports_the_first_missing_variable() {
        let err = Config::from_vars(vars(&ALL[1..])).unwrap_err();
        assert_eq!(err, ConfigError::Missing(BASE_URL_VAR));

        let without_token = &ALL[..2];
        let err = Config::from_vars(vars(without_token)).unwrap_err();
        assert_eq!(err, ConfigError::Missing(API_TOKEN_VAR));
    }

    #[test]
    fn treats_empty_and_whitespace_values_as_missing_content() {
        let all = [
            (BASE_URL_VAR, "https://example.atlassian.net"),
            (EMAIL_VAR, "  "),
            (API_TOKEN_VAR, "token"),
        ];
        assert_eq!(
            Config::from_vars(vars(&all)).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );
    }

    #[test]
    fn rejects_a_base_url_that_is_not_parseable() {
        let all = [
            (BASE_URL_VAR, "not a url"),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
        ];
        assert_eq!(
            Config::from_vars(vars(&all)).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn rejects_a_base_url_with_a_non_http_scheme() {
        let all = [
            (BASE_URL_VAR, "ftp://example.com"),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
        ];
        assert_eq!(
            Config::from_vars(vars(&all)).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn rejects_plaintext_http_by_default() {
        for base_url in ["http://example.com", "http://127.0.0.1:8080"] {
            let all = [
                (BASE_URL_VAR, base_url),
                (EMAIL_VAR, "jane@example.com"),
                (API_TOKEN_VAR, "token"),
            ];

            assert_eq!(
                Config::from_vars(vars(&all)).unwrap_err(),
                ConfigError::InvalidBaseUrl(BASE_URL_VAR)
            );
        }
    }

    #[test]
    fn explicit_insecure_http_is_limited_to_loopback_testing() {
        for base_url in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            let all = [
                (BASE_URL_VAR, base_url),
                (EMAIL_VAR, "jane@example.com"),
                (API_TOKEN_VAR, "token"),
                (ALLOW_INSECURE_HTTP_VAR, "1"),
            ];

            assert!(Config::from_vars(vars(&all)).is_ok());
        }

        let remote = [
            (BASE_URL_VAR, "http://example.com"),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
            (ALLOW_INSECURE_HTTP_VAR, "1"),
        ];
        assert_eq!(
            Config::from_vars(vars(&remote)).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn rejects_base_url_credentials_without_echoing_them() {
        let secret = "embedded-secret";
        let all = [
            (
                BASE_URL_VAR,
                "https://jira-user:embedded-secret@example.com/jira",
            ),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
        ];

        let error = Config::from_vars(vars(&all)).unwrap_err();

        assert_eq!(error, ConfigError::InvalidBaseUrl(BASE_URL_VAR));
        assert!(!error.to_string().contains(secret));
    }
}

use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

pub const BASE_URL_VAR: &str = "JIRA_BASE_URL";
pub const EMAIL_VAR: &str = "JIRA_EMAIL";
pub const API_TOKEN_VAR: &str = "JIRA_API_TOKEN";

/// Everything jcli needs to authenticate against a Jira Cloud instance:
/// the instance base URL plus an email and API token for Basic auth.
///
/// The manual `Debug` implementation prints the base URL only; the email
/// and API token never reach debug output.
pub struct Config {
    pub base_url: Url,
    pub email: String,
    pub api_token: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("missing configuration value {0}")]
    Missing(&'static str),
    #[error("configuration value {0} is set but empty")]
    Empty(&'static str),
    #[error("invalid URL in {0}")]
    InvalidBaseUrl(&'static str),
    #[error("configuration file exists but cannot be read")]
    Unreadable,
    #[error("configuration file is malformed")]
    Malformed,
    #[error("the project .env file is invalid")]
    DotEnvInvalid,
}

/// One setting's value within a single source. Presence is tracked even when
/// the value is unusable (blank or non-Unicode), because a present value in
/// a higher-priority source is authoritative and must not silently fall
/// through to a lower-priority one.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Lookup {
    Absent,
    NonUnicode,
    Value(String),
}

impl Lookup {
    fn from_os(value: Option<OsString>) -> Self {
        match value {
            None => Lookup::Absent,
            Some(value) => match value.into_string() {
                Ok(value) => Lookup::Value(value),
                Err(_) => Lookup::NonUnicode,
            },
        }
    }
}

impl Config {
    /// Loads configuration from up to three sources.
    ///
    /// Precedence per setting is process environment, then the exact `.env`
    /// in the current working directory, then the per-user TOML file. A
    /// present value in a higher-priority source always wins, even when it
    /// is blank or non-Unicode; only an absent value falls through.
    ///
    /// The `.env` is parsed without mutating the process environment so
    /// values keep their source provenance.
    pub fn load() -> Result<Self, ConfigError> {
        let dot_env = read_dot_env(Path::new(".env"))?;
        let file = read_config_file()?;
        Self::from_sources(
            |name| Lookup::from_os(env::var_os(name)),
            dot_env.as_ref(),
            file.as_ref(),
        )
    }

    /// Testable core of `load`: takes an environment lookup, the `.env`
    /// values, and the parsed TOML values so tests never touch the real
    /// environment or filesystem.
    fn from_sources(
        env: impl Fn(&str) -> Lookup,
        dot_env: Option<&BTreeMap<String, String>>,
        file: Option<&FileValues>,
    ) -> Result<Self, ConfigError> {
        let base_url = resolved(&env, dot_env, file, BASE_URL_VAR)?;
        Ok(Config {
            base_url: normalize_base_url(&base_url)?,
            email: resolved(&env, dot_env, file, EMAIL_VAR)?,
            api_token: resolved(&env, dot_env, file, API_TOKEN_VAR)?,
        })
    }
}

/// Picks one credential setting: environment over `.env` over TOML. A
/// present-but-blank or non-Unicode value in a source is authoritative and
/// reports `Empty` instead of falling through; only absence falls through.
/// Blank TOML values keep their presence, so they report `Empty` while
/// omitted keys report `Missing`.
fn resolved(
    env: &impl Fn(&str) -> Lookup,
    dot_env: Option<&BTreeMap<String, String>>,
    file: Option<&FileValues>,
    name: &'static str,
) -> Result<String, ConfigError> {
    match env(name) {
        Lookup::Value(value) => content_or_empty(value, name),
        Lookup::NonUnicode => Err(ConfigError::Empty(name)),
        Lookup::Absent => {
            if let Some(value) = dot_env.and_then(|values| values.get(name)) {
                return content_or_empty(value.clone(), name);
            }
            match file.and_then(|file| file.get(name)) {
                Some(value) => content_or_empty(value.to_owned(), name),
                None => Err(ConfigError::Missing(name)),
            }
        }
    }
}

fn content_or_empty(value: String, name: &'static str) -> Result<String, ConfigError> {
    // A present-but-blank value must not fall through to a lower-priority
    // source: that could combine credentials from two different sources.
    if has_content(&value) {
        Ok(value)
    } else {
        Err(ConfigError::Empty(name))
    }
}

/// Flat schema of `config.toml`. Keys are lowercase and optional because the
/// environment may supply any subset. Unknown keys are rejected so typos
/// fail loudly instead of silently disabling a setting.
///
/// The manual `Debug` implementation prints the base URL only; the email
/// and API token never reach debug output, even in test assertion
/// messages.
#[derive(Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileValues {
    base_url: Option<String>,
    email: Option<String>,
    api_token: Option<String>,
}

impl fmt::Debug for FileValues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileValues")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl FileValues {
    fn get(&self, name: &str) -> Option<&str> {
        match name {
            BASE_URL_VAR => self.base_url.as_deref(),
            EMAIL_VAR => self.email.as_deref(),
            API_TOKEN_VAR => self.api_token.as_deref(),
            _ => None,
        }
    }
}

/// Reads the exact `.env` in the given directory — no ancestor search —
/// without mutating the process environment. A missing file is `None`.
/// Duplicate keys keep the first assignment, matching `dotenvy::from_path`
/// (and `dotenv()`); `from_path_override` is the last-wins variant and is
/// not what jcli uses.
fn read_dot_env(path: &Path) -> Result<Option<BTreeMap<String, String>>, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfigError::Unreadable),
    };
    // `dotenvy::Iter::load` removes this marker before parsing. Keep the
    // same behavior while parsing without mutating the process environment.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let contents = std::str::from_utf8(bytes).map_err(|_| ConfigError::DotEnvInvalid)?;
    let mut values = BTreeMap::new();
    for entry in dotenvy::Iter::new(contents.as_bytes()) {
        let Ok((key, value)) = entry else {
            return Err(ConfigError::DotEnvInvalid);
        };
        values.entry(key).or_insert(value);
    }
    Ok(Some(values))
}

/// Reads and parses the optional per-user TOML configuration file. Only the
/// per-user path is used; there is no system-wide fallback. A missing file
/// is normal and yields `None`; anything else wrong with a present file is
/// a generic error that never echoes its contents.
fn read_config_file() -> Result<Option<FileValues>, ConfigError> {
    let Some(path) = discover_config_path() else {
        return Ok(None);
    };
    read_config_file_at(&path)
}

fn read_config_file_at(path: &Path) -> Result<Option<FileValues>, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfigError::Unreadable),
    };
    let contents = String::from_utf8(bytes).map_err(|_| ConfigError::Malformed)?;
    parse_config_file(&contents).map(Some)
}

fn parse_config_file(contents: &str) -> Result<FileValues, ConfigError> {
    toml::from_str(contents).map_err(|_| ConfigError::Malformed)
}

/// Picks the per-user configuration file: `$XDG_CONFIG_HOME/jcli/config.toml`
/// when `XDG_CONFIG_HOME` is set, non-empty and absolute, otherwise
/// `~/.config/jcli/config.toml` when the home directory is absolute. A
/// relative value would resolve against the current working directory and
/// is therefore never used as a credential path.
fn discover_config_path() -> Option<PathBuf> {
    discover_config_path_with(
        env::var_os("XDG_CONFIG_HOME").as_deref(),
        env::var_os("HOME").as_deref(),
    )
}

fn discover_config_path_with(xdg: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    if let Some(config_home) = xdg.filter(|value| !value.is_empty()) {
        let config_home = Path::new(config_home);
        if config_home.is_absolute() {
            return Some(config_home.join("jcli").join("config.toml"));
        }
    }
    let home = Path::new(home?);
    if !home.is_absolute() {
        return None;
    }
    Some(home.join(".config").join("jcli").join("config.toml"))
}

fn has_content(value: &str) -> bool {
    !value.trim().is_empty()
}

/// Ensures the base URL ends in `/` so that `Url::join` with a relative API
/// path appends to the path instead of replacing its last segment:
/// `https://x/browse` + `rest/...` would otherwise lose `/browse`.
fn normalize_base_url(raw: &str) -> Result<Url, ConfigError> {
    let mut url = Url::parse(raw).map_err(|_| ConfigError::InvalidBaseUrl(BASE_URL_VAR))?;
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn vars<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Lookup + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| Lookup::Value((*value).to_owned()))
                .unwrap_or(Lookup::Absent)
        }
    }

    fn absent(_: &str) -> Lookup {
        Lookup::Absent
    }

    fn dot_env_values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    const ALL: &[(&str, &str)] = &[
        (BASE_URL_VAR, "https://example.atlassian.net"),
        (EMAIL_VAR, "jane@example.com"),
        (API_TOKEN_VAR, "secret-token"),
    ];

    fn file_values(contents: &str) -> FileValues {
        parse_config_file(contents).unwrap()
    }

    const FULL_FILE: &str = "base_url = \"https://file.example.com\"\n\
                             email = \"file@example.com\"\n\
                             api_token = \"file-token\"\n";

    #[test]
    fn loads_all_three_variables() {
        let config = Config::from_sources(vars(ALL), None, None).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.atlassian.net/");
        assert_eq!(config.email, "jane@example.com");
        assert_eq!(config.api_token, "secret-token");
    }

    #[test]
    fn keeps_a_base_url_with_a_path_prefix_intact() {
        let mut all = ALL.to_vec();
        all[0] = (BASE_URL_VAR, "https://example.com/jira");
        let config = Config::from_sources(vars(&all), None, None).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.com/jira/");
    }

    #[test]
    fn accepts_a_plaintext_url_that_parses() {
        let mut all = ALL.to_vec();
        all[0] = (BASE_URL_VAR, "http://localhost:8080");
        let config = Config::from_sources(vars(&all), None, None).unwrap();
        assert_eq!(config.base_url.as_str(), "http://localhost:8080/");
    }

    #[test]
    fn reports_the_first_missing_variable() {
        let err = Config::from_sources(vars(&ALL[1..]), None, None).unwrap_err();
        assert_eq!(err, ConfigError::Missing(BASE_URL_VAR));

        let without_token = &ALL[..2];
        let err = Config::from_sources(vars(without_token), None, None).unwrap_err();
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
            Config::from_sources(vars(&all), None, None).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );
    }

    #[test]
    fn a_blank_environment_value_is_authoritative_and_never_falls_through() {
        let file = file_values(FULL_FILE);
        let dot_env = dot_env_values(&[(EMAIL_VAR, "env@example.com")]);
        let all = [
            (BASE_URL_VAR, "https://example.atlassian.net"),
            (EMAIL_VAR, ""),
            (API_TOKEN_VAR, "token"),
        ];

        assert_eq!(
            Config::from_sources(vars(&all), Some(&dot_env), Some(&file)).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );
    }

    #[test]
    fn a_non_unicode_environment_value_is_authoritative_and_never_falls_through() {
        let file = file_values(FULL_FILE);
        let env = |name: &str| match name {
            EMAIL_VAR => Lookup::NonUnicode,
            BASE_URL_VAR => Lookup::Value("https://example.atlassian.net".to_owned()),
            API_TOKEN_VAR => Lookup::Value("token".to_owned()),
            _ => Lookup::Absent,
        };

        assert_eq!(
            Config::from_sources(env, None, Some(&file)).unwrap_err(),
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
            Config::from_sources(vars(&all), None, None).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
        assert_eq!(
            ConfigError::InvalidBaseUrl(BASE_URL_VAR).to_string(),
            "invalid URL in JIRA_BASE_URL"
        );
    }

    #[test]
    fn debug_output_never_contains_credentials() {
        let config = Config::from_sources(vars(ALL), None, None).unwrap();
        let rendered = format!("{config:?}");
        assert!(rendered.contains("base_url"));
        assert!(!rendered.contains("jane@example.com"));
        assert!(!rendered.contains("secret-token"));

        let file = file_values(FULL_FILE);
        let rendered = format!("{file:?}");
        assert!(!rendered.contains("file@example.com"));
        assert!(!rendered.contains("file-token"));
    }

    #[test]
    fn parses_the_flat_schema() {
        let file = file_values(FULL_FILE);
        assert_eq!(file.base_url.as_deref(), Some("https://file.example.com"));
        assert_eq!(file.email.as_deref(), Some("file@example.com"));
        assert_eq!(file.api_token.as_deref(), Some("file-token"));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert_eq!(
            parse_config_file("base_ur1 = \"https://x\"\n").unwrap_err(),
            ConfigError::Malformed
        );
        assert_eq!(
            parse_config_file("[table]\nkey = \"x\"\n").unwrap_err(),
            ConfigError::Malformed
        );
    }

    #[test]
    fn malformed_file_is_a_generic_error_without_file_contents() {
        let secret = "token-value";
        let contents = format!("base_url = \"https://x\"\nemail = 123\napi_token = \"{secret}\"\n");

        let error = parse_config_file(&contents).unwrap_err();

        assert_eq!(error, ConfigError::Malformed);
        let message = error.to_string();
        assert!(!message.contains(secret));
        assert!(!message.contains("email = 123"));
        assert!(!message.contains("expected"));
    }

    #[test]
    fn environment_values_override_dot_env_and_the_file() {
        let file = file_values(FULL_FILE);
        let dot_env = dot_env_values(&[
            (BASE_URL_VAR, "https://dotenv.example.com"),
            (EMAIL_VAR, "dotenv@example.com"),
            (API_TOKEN_VAR, "dotenv-token"),
        ]);
        let config = Config::from_sources(vars(ALL), Some(&dot_env), Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.atlassian.net/");
        assert_eq!(config.email, "jane@example.com");
        assert_eq!(config.api_token, "secret-token");
    }

    #[test]
    fn dot_env_values_override_the_file() {
        let file = file_values(FULL_FILE);
        let dot_env = dot_env_values(&[
            (BASE_URL_VAR, "https://dotenv.example.com"),
            (EMAIL_VAR, "dotenv@example.com"),
            (API_TOKEN_VAR, "dotenv-token"),
        ]);

        let config = Config::from_sources(absent, Some(&dot_env), Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://dotenv.example.com/");
        assert_eq!(config.email, "dotenv@example.com");
        assert_eq!(config.api_token, "dotenv-token");
    }

    #[test]
    fn the_file_supplies_values_only_when_the_dot_env_supplies_none() {
        let file = file_values(FULL_FILE);
        let config = Config::from_sources(vars(&ALL[..1]), None, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.atlassian.net/");
        assert_eq!(config.email, "file@example.com");
        assert_eq!(config.api_token, "file-token");
    }

    #[test]
    fn file_only_configuration() {
        let file = file_values(FULL_FILE);
        let config = Config::from_sources(absent, None, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://file.example.com/");
        assert_eq!(config.email, "file@example.com");
        assert_eq!(config.api_token, "file-token");
    }

    #[test]
    fn an_empty_dot_env_file_keeps_the_file_active() {
        let file = file_values(FULL_FILE);
        let dot_env = BTreeMap::new();

        let config = Config::from_sources(absent, Some(&dot_env), Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://file.example.com/");
    }

    #[test]
    fn a_blank_dot_env_value_is_authoritative() {
        let file = file_values(FULL_FILE);
        let dot_env = dot_env_values(&[(EMAIL_VAR, "   ")]);
        let all = [
            (BASE_URL_VAR, "https://example.atlassian.net"),
            (API_TOKEN_VAR, "token"),
        ];

        assert_eq!(
            Config::from_sources(vars(&all), Some(&dot_env), Some(&file)).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );
    }

    #[test]
    fn an_invalid_dot_env_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".env"),
            "JIRA_EMAIL=partial@example.com\nnot a valid line\n",
        )
        .unwrap();

        assert_eq!(
            read_dot_env(&dir.path().join(".env")).unwrap_err(),
            ConfigError::DotEnvInvalid
        );
        assert_eq!(
            ConfigError::DotEnvInvalid.to_string(),
            "the project .env file is invalid"
        );
    }

    #[test]
    fn an_unreadable_dot_env_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        fs::create_dir(&path).unwrap();

        assert_eq!(read_dot_env(&path).unwrap_err(), ConfigError::Unreadable);
    }

    #[test]
    fn a_value_missing_from_all_sources_is_a_configuration_error() {
        let file = file_values("base_url = \"https://file.example.com\"\n");
        assert_eq!(
            Config::from_sources(absent, None, Some(&file)).unwrap_err(),
            ConfigError::Missing(EMAIL_VAR)
        );
    }

    #[test]
    fn a_blank_file_value_reports_empty_while_an_omitted_key_reports_missing() {
        let file = file_values("base_url = \"https://file.example.com\"\nemail = \"\"\n");
        assert_eq!(
            Config::from_sources(absent, None, Some(&file)).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );

        let file = file_values("base_url = \"https://file.example.com\"\n");
        assert_eq!(
            Config::from_sources(absent, None, Some(&file)).unwrap_err(),
            ConfigError::Missing(EMAIL_VAR)
        );
    }

    #[test]
    fn file_values_are_validated_like_environment_values() {
        let file = file_values("base_url = \"not a url\"\n");
        assert_eq!(
            Config::from_sources(absent, None, Some(&file)).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn reads_the_exact_dot_env_file_without_touching_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.path().join(".env"), "JIRA_EMAIL=root@example.com\n").unwrap();

        assert!(read_dot_env(&nested.join(".env")).unwrap().is_none());

        let values = read_dot_env(&dir.path().join(".env")).unwrap().unwrap();
        assert_eq!(
            values.get(EMAIL_VAR).map(String::as_str),
            Some("root@example.com")
        );
    }

    #[test]
    fn a_missing_dot_env_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_dot_env(&dir.path().join(".env")).unwrap().is_none());
    }

    #[test]
    fn duplicate_dot_env_keys_keep_the_first_assignment() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".env"),
            "JIRA_EMAIL=first@example.com\nJIRA_EMAIL=second@example.com\n",
        )
        .unwrap();

        let values = read_dot_env(&dir.path().join(".env")).unwrap().unwrap();
        assert_eq!(
            values.get(EMAIL_VAR).map(String::as_str),
            Some("first@example.com")
        );
    }

    #[test]
    fn a_bom_prefixed_dot_env_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"JIRA_EMAIL=bom@example.com\n");
        fs::write(dir.path().join(".env"), bytes).unwrap();

        let values = read_dot_env(&dir.path().join(".env")).unwrap().unwrap();
        assert_eq!(
            values.get(EMAIL_VAR).map(String::as_str),
            Some("bom@example.com")
        );
    }

    #[test]
    fn an_absolute_xdg_config_home_wins() {
        assert_eq!(
            discover_config_path_with(
                Some(OsStr::new("/xdg/config")),
                Some(OsStr::new("/home/jane")),
            ),
            Some(PathBuf::from("/xdg/config/jcli/config.toml"))
        );
    }

    #[test]
    fn an_unset_or_empty_xdg_config_home_uses_the_home_directory() {
        let expected = Some(PathBuf::from("/home/jane/.config/jcli/config.toml"));
        assert_eq!(
            discover_config_path_with(None, Some(OsStr::new("/home/jane"))),
            expected
        );
        assert_eq!(
            discover_config_path_with(Some(OsStr::new("")), Some(OsStr::new("/home/jane"))),
            expected
        );
    }

    #[test]
    fn a_relative_xdg_config_home_is_ignored_rather_than_resolved_against_cwd() {
        assert_eq!(
            discover_config_path_with(
                Some(OsStr::new("xdg/config")),
                Some(OsStr::new("/home/jane"))
            ),
            Some(PathBuf::from("/home/jane/.config/jcli/config.toml"))
        );
        assert_eq!(
            discover_config_path_with(Some(OsStr::new("./xdg/config")), None),
            None
        );
    }

    #[test]
    fn a_relative_home_directory_is_never_used() {
        assert_eq!(
            discover_config_path_with(None, Some(OsStr::new("home/jane"))),
            None
        );
    }

    #[test]
    fn without_any_usable_home_there_is_no_config_file() {
        assert_eq!(discover_config_path_with(None, None), None);
    }

    fn write_file(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_missing_config_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            read_config_file_at(&dir.path().join("jcli/config.toml")).unwrap(),
            None
        );
    }

    #[test]
    fn a_present_config_file_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_file(&path, FULL_FILE);

        let file = read_config_file_at(&path).unwrap().unwrap();
        assert_eq!(file.email.as_deref(), Some("file@example.com"));
    }

    #[test]
    fn a_non_regular_target_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config-dir");
        fs::create_dir(&path).unwrap();

        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::Unreadable
        );
    }

    #[test]
    fn a_non_utf8_file_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, b"email = \"\xff\xfe\"\n").unwrap();

        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::Malformed
        );
    }

    #[test]
    fn an_unparsable_file_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_file(&path, "email = \n");

        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::Malformed
        );
    }

    #[test]
    fn file_errors_stay_generic() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "secret-token";
        let path = dir.path().join("config.toml");
        write_file(&path, &format!("api_token = \"{secret}\"\nemail = ]\n"));

        let message = read_config_file_at(&path).unwrap_err().to_string();
        assert!(!message.contains(secret));
        assert!(!message.contains(dir.path().to_string_lossy().as_ref()));
        assert!(!message.contains("expected"));
    }
}

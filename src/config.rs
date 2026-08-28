use std::collections::BTreeMap;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::File;
#[cfg(unix)]
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;
use url::{Host, Url};

pub const BASE_URL_VAR: &str = "JIRA_BASE_URL";
pub const EMAIL_VAR: &str = "JIRA_EMAIL";
pub const API_TOKEN_VAR: &str = "JIRA_API_TOKEN";
const ALLOW_INSECURE_HTTP_VAR: &str = "JCLI_ALLOW_INSECURE_HTTP";

/// Upper bound for the configuration files. They hold a handful of short
/// strings, so anything larger is not a file jcli ever intended to read.
/// The same bound applies to the `.env` file so both reads are bounded.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// Every setting jcli reads from the environment or `.env`. The insecure
/// loopback flag deliberately has no TOML counterpart: it relaxes transport
/// security and must stay an explicit environment/`.env` decision.
const SETTING_VARS: [&str; 4] = [
    BASE_URL_VAR,
    EMAIL_VAR,
    API_TOKEN_VAR,
    ALLOW_INSECURE_HTTP_VAR,
];

/// The three settings every command needs. Presence in the process
/// environment is enough — a present-but-blank value is authoritative and
/// fails on its own, so the TOML file is irrelevant at that point.
const REQUIRED_VARS: [&str; 3] = [BASE_URL_VAR, EMAIL_VAR, API_TOKEN_VAR];

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
    #[error("invalid URL in {0}: expected an HTTPS URL without embedded credentials")]
    InvalidBaseUrl(&'static str),
    #[error("configuration file exists but cannot be read")]
    Unreadable,
    #[error("configuration file is malformed")]
    Malformed,
    #[error("configuration file is too large")]
    TooLarge,
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

/// Outcome of reading the exact-CWD `.env`. Absence, usable values, and a
/// present-but-unclassifiable file are three distinct states: a broken,
/// unreadable, oversized, or non-regular `.env` must never be mistaken for
/// a missing one, or the TOML file would quietly come back into play.
enum DotEnv {
    Absent,
    Invalid,
    Values(BTreeMap<String, String>),
}

impl DotEnv {
    fn lookup(&self, name: &str) -> Lookup {
        match self {
            DotEnv::Values(values) => values
                .get(name)
                .map(|value| Lookup::Value(value.clone()))
                .unwrap_or(Lookup::Absent),
            DotEnv::Absent | DotEnv::Invalid => Lookup::Absent,
        }
    }

    /// True when this `.env` state switches the TOML file off: either it
    /// supplies any jcli-relevant setting, so credentials and base URL
    /// cannot be assembled across the two sources, or it is present but
    /// unclassifiable, which disables TOML just the same.
    fn disables_toml(&self) -> bool {
        match self {
            DotEnv::Absent => false,
            DotEnv::Invalid => true,
            DotEnv::Values(values) => SETTING_VARS.iter().any(|name| values.contains_key(*name)),
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
    /// Source mixing is prevented: as soon as the working-directory `.env`
    /// supplies any jcli-relevant setting — or exists but cannot be
    /// classified — the TOML file is ignored entirely, so file credentials
    /// never attach to a project-selected base URL and vice versa. The
    /// `.env` is parsed without mutating the process environment to keep
    /// that provenance visible. The TOML file is not even read when the
    /// process environment already supplies all required settings, so an
    /// unused malformed file cannot block a complete process
    /// configuration.
    pub fn load() -> Result<Self, ConfigError> {
        let dot_env = read_dot_env(Path::new(".env"));
        let env_is_complete = env_supplies_all_required();
        if matches!(dot_env, DotEnv::Invalid) && !env_is_complete {
            return Err(ConfigError::DotEnvInvalid);
        }
        let file = if toml_participates(&dot_env, env_is_complete) {
            read_config_file()?
        } else {
            None
        };
        Self::from_sources(
            |name| Lookup::from_os(env::var_os(name)),
            &dot_env,
            file.as_ref(),
        )
    }

    /// Testable core of `load`: takes an environment lookup, the `.env`
    /// state, and the parsed TOML values so tests never touch the real
    /// environment or filesystem.
    fn from_sources(
        env: impl Fn(&str) -> Lookup,
        dot_env: &DotEnv,
        file: Option<&FileValues>,
    ) -> Result<Self, ConfigError> {
        let file = if dot_env.disables_toml() { None } else { file };
        let base_url = resolved(&env, dot_env, file, BASE_URL_VAR)?;
        let allow_insecure_loopback = allow_insecure_http(&env, dot_env);
        let base_url = normalize_base_url(&base_url, allow_insecure_loopback)?;
        Ok(Config {
            base_url,
            email: resolved(&env, dot_env, file, EMAIL_VAR)?,
            api_token: resolved(&env, dot_env, file, API_TOKEN_VAR)?,
        })
    }
}

/// Decides whether the TOML file participates in `load` at all. It is
/// skipped when the `.env` policy disables it, and when the process
/// environment already supplies every required Jira setting, because then
/// no value can ever come from the file.
fn toml_participates(dot_env: &DotEnv, env_supplies_all_required: bool) -> bool {
    !dot_env.disables_toml() && !env_supplies_all_required
}

fn env_supplies_all_required() -> bool {
    REQUIRED_VARS.iter().all(|name| env::var_os(name).is_some())
}

/// Picks one credential setting: environment over `.env` over TOML. A
/// present-but-blank or non-Unicode value in a source is authoritative and
/// reports `Empty` instead of falling through; only absence falls through.
/// Blank TOML values keep their presence, so they report `Empty` while
/// omitted keys report `Missing`.
fn resolved(
    env: &impl Fn(&str) -> Lookup,
    dot_env: &DotEnv,
    file: Option<&FileValues>,
    name: &'static str,
) -> Result<String, ConfigError> {
    match env(name) {
        Lookup::Value(value) => content_or_empty(value, name),
        Lookup::NonUnicode => Err(ConfigError::Empty(name)),
        Lookup::Absent => match dot_env.lookup(name) {
            Lookup::Value(value) => content_or_empty(value, name),
            Lookup::NonUnicode => Err(ConfigError::Empty(name)),
            Lookup::Absent => file
                .and_then(|file| file.get(name).map(str::to_owned))
                .map(|value| content_or_empty(value, name))
                .unwrap_or_else(|| Err(ConfigError::Missing(name))),
        },
    }
}

fn content_or_empty(value: String, name: &'static str) -> Result<String, ConfigError> {
    if has_content(&value) {
        Ok(value)
    } else {
        Err(ConfigError::Empty(name))
    }
}

/// The insecure-loopback opt-in comes from the process environment or the
/// `.env`, never from TOML. An environment value, including one that
/// disagrees, shadows the `.env` value.
fn allow_insecure_http(env: &impl Fn(&str) -> Lookup, dot_env: &DotEnv) -> bool {
    match env(ALLOW_INSECURE_HTTP_VAR) {
        Lookup::Value(value) => value == "1",
        Lookup::NonUnicode => false,
        Lookup::Absent => {
            matches!(dot_env.lookup(ALLOW_INSECURE_HTTP_VAR), Lookup::Value(v) if v == "1")
        }
    }
}

/// Flat schema of `config.toml`. Keys are lowercase and optional because the
/// environment may supply any subset. Unknown keys are rejected so typos
/// fail loudly instead of silently disabling a setting; the insecure
/// loopback flag is not part of the schema on purpose.
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

/// Reads the exact `.env` in the given directory — no ancestor search — as
/// a bounded regular-file read, without mutating the process environment,
/// so values keep their source provenance. A missing file is `Absent`.
/// Anything else wrong with a present file — unreadable, non-regular,
/// oversized, invalid UTF-8, or a malformed line — is `Invalid`: that
/// disables the TOML file and yields no partial entries, because a
/// half-parsed `.env` must not look like an absent one.
///
/// On Unix the final path component must not be a symbolic link; see
/// `open_bounded`.
fn read_dot_env(path: &Path) -> DotEnv {
    match open_bounded(path) {
        Ok(None) => DotEnv::Absent,
        Ok(Some(bytes)) => parse_dot_env(&bytes),
        Err(_) => DotEnv::Invalid,
    }
}

/// Parses `.env` bytes with dotenvy's own parser. Duplicate keys keep the
/// first assignment, matching `dotenvy::from_path` (and `dotenv()`);
/// `from_path_override` is the last-wins variant and is not what jcli
/// uses. Active variable substitution is rejected because expansion could
/// exceed the bounded input size. Literal dollars in single-quoted values,
/// escaped values, and comments remain valid. Any parse error discards all
/// entries.
fn parse_dot_env(bytes: &[u8]) -> DotEnv {
    // `dotenvy::Iter::load` removes this marker before parsing. Keep the same
    // behavior while parsing without mutating the process environment.
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    let Ok(contents) = std::str::from_utf8(bytes) else {
        return DotEnv::Invalid;
    };
    if contains_active_substitution(contents) {
        return DotEnv::Invalid;
    }

    let mut values = BTreeMap::new();
    for entry in dotenvy::Iter::new(contents.as_bytes()) {
        let Ok((key, value)) = entry else {
            return DotEnv::Invalid;
        };
        values.entry(key).or_insert(value);
    }
    DotEnv::Values(values)
}

/// Detects substitutions before dotenvy expands them. The scan follows the
/// value syntax enough to distinguish active `$` markers from escaped dollars,
/// single-quoted literals, and comments. Quote state carries across physical
/// lines because dotenvy permits multiline quoted values.
fn contains_active_substitution(contents: &str) -> bool {
    let mut at_line_start = true;
    let mut has_equal = false;
    let mut value_started = false;
    let mut strong_quote = false;
    let mut weak_quote = false;
    let mut escaped = false;
    let mut expecting_end = false;
    let mut in_comment = false;

    for character in contents.chars() {
        if character == '\n' {
            if !strong_quote && !weak_quote {
                at_line_start = true;
                has_equal = false;
                value_started = false;
                escaped = false;
                expecting_end = false;
                in_comment = false;
            } else {
                at_line_start = false;
            }
            continue;
        }
        if in_comment {
            continue;
        }
        if escaped {
            escaped = false;
            at_line_start = false;
            continue;
        }
        if strong_quote {
            if character == '\'' {
                strong_quote = false;
            }
            continue;
        }
        if weak_quote {
            match character {
                '\\' => escaped = true,
                '"' => weak_quote = false,
                '$' => return true,
                _ => {}
            }
            continue;
        }
        if at_line_start {
            if character.is_whitespace() {
                continue;
            }
            if character == '#' {
                in_comment = true;
                continue;
            }
            at_line_start = false;
        }
        if !has_equal {
            if character == '=' {
                has_equal = true;
                value_started = false;
            }
            continue;
        }
        if expecting_end {
            match character {
                ' ' | '\t' => continue,
                '#' => {
                    in_comment = true;
                    continue;
                }
                _ => continue,
            }
        }
        if !value_started {
            if character.is_whitespace() {
                continue;
            }
            if character == '#' {
                in_comment = true;
                continue;
            }
            value_started = true;
        }
        match character {
            '\\' => escaped = true,
            '\'' => strong_quote = true,
            '"' => weak_quote = true,
            '$' => return true,
            ' ' | '\t' => expecting_end = true,
            _ => {}
        }
    }
    false
}

/// Locates the optional per-user TOML configuration file and parses it.
/// Only the per-user path is used; there is no system-wide fallback. A
/// missing file is normal and yields `None`; anything else wrong with a
/// present file is a generic error that never echoes its contents.
fn read_config_file() -> Result<Option<FileValues>, ConfigError> {
    let Some(path) = discover_config_path() else {
        return Ok(None);
    };
    read_config_file_at(&path)
}

/// Opens and reads one candidate configuration path, classifying failures:
/// `NotFound` means the optional file is absent, non-regular or unreadable
/// targets report `Unreadable`, and a file that exceeds the size cap
/// reports `TooLarge`. Raw bytes are read before any UTF-8 decoding, so a
/// file whose bytes past the cap are invalid UTF-8 is still classified by
/// size, not reported as malformed.
///
/// On Unix the final path component must not be a symbolic link: the file
/// is opened with `O_NOFOLLOW`, and a symlink target reports `Unreadable`
/// for the TOML file and `Invalid` for the `.env`. This keeps a swapped
/// link from silently redirecting credential reads.
fn read_config_file_at(path: &Path) -> Result<Option<FileValues>, ConfigError> {
    let Some(bytes) = open_bounded(path)? else {
        return Ok(None);
    };
    let contents = String::from_utf8(bytes).map_err(|_| ConfigError::Malformed)?;
    parse_config_file(&contents).map(Some)
}

/// Opens one candidate path and reads at most `MAX_CONFIG_BYTES + 1` raw
/// bytes from it. `Ok(None)` means the target is absent.
///
/// On Unix the file is opened with `O_NONBLOCK | O_NOFOLLOW`: a FIFO or
/// device file cannot block the open, and a symbolic link as the final
/// path component is rejected rather than followed. Regular-file status is
/// then validated on the opened handle before a single byte is read, so
/// the read only ever happens on a verified regular file. The flags stay
/// set for the read itself, which is a no-op for regular files. Non-Unix
/// platforms keep a portable `File::open` with the same handle-level
/// regular-file check.
fn open_bounded(path: &Path) -> Result<Option<Vec<u8>>, ConfigError> {
    let mut file = match open_config_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ConfigError::Unreadable),
    };
    let metadata = file.metadata().map_err(|_| ConfigError::Unreadable)?;
    if !metadata.is_file() {
        return Err(ConfigError::Unreadable);
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::TooLarge);
    }

    let mut bytes = Vec::new();
    let read = file
        .by_ref()
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes);
    match read {
        Ok(_) if bytes.len() > MAX_CONFIG_BYTES as usize => Err(ConfigError::TooLarge),
        Ok(_) => Ok(Some(bytes)),
        Err(_) => Err(ConfigError::Unreadable),
    }
}

#[cfg(unix)]
fn open_config_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_config_file(path: &Path) -> io::Result<File> {
    File::open(path)
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

    fn dot_env_values(pairs: &[(&str, &str)]) -> DotEnv {
        DotEnv::Values(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        )
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
        let config = Config::from_sources(vars(ALL), &DotEnv::Absent, None).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.atlassian.net/");
        assert_eq!(config.email, "jane@example.com");
        assert_eq!(config.api_token, "secret-token");
    }

    #[test]
    fn keeps_a_base_url_with_a_path_prefix_intact() {
        let mut all = ALL.to_vec();
        all[0] = (BASE_URL_VAR, "https://example.com/jira");
        let config = Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.com/jira/");
    }

    #[test]
    fn reports_the_first_missing_variable() {
        let err = Config::from_sources(vars(&ALL[1..]), &DotEnv::Absent, None).unwrap_err();
        assert_eq!(err, ConfigError::Missing(BASE_URL_VAR));

        let without_token = &ALL[..2];
        let err = Config::from_sources(vars(without_token), &DotEnv::Absent, None).unwrap_err();
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
            Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap_err(),
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
            Config::from_sources(vars(&all), &dot_env, Some(&file)).unwrap_err(),
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
            Config::from_sources(env, &DotEnv::Absent, Some(&file)).unwrap_err(),
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
            Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap_err(),
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
            Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap_err(),
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
                Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap_err(),
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

            assert!(Config::from_sources(vars(&all), &DotEnv::Absent, None).is_ok());
        }

        let remote = [
            (BASE_URL_VAR, "http://example.com"),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
            (ALLOW_INSECURE_HTTP_VAR, "1"),
        ];
        assert_eq!(
            Config::from_sources(vars(&remote), &DotEnv::Absent, None).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn the_insecure_http_flag_may_come_from_the_dot_env() {
        let all = [
            (BASE_URL_VAR, "http://localhost:8080"),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
        ];
        let dot_env = dot_env_values(&[(ALLOW_INSECURE_HTTP_VAR, "1")]);

        let config = Config::from_sources(vars(&all), &dot_env, None).unwrap();
        assert_eq!(config.base_url.scheme(), "http");
    }

    #[test]
    fn an_environment_flag_shadows_a_conflicting_dot_env_flag() {
        let all = [
            (BASE_URL_VAR, "http://localhost:8080"),
            (EMAIL_VAR, "jane@example.com"),
            (API_TOKEN_VAR, "token"),
            (ALLOW_INSECURE_HTTP_VAR, "0"),
        ];
        let dot_env = dot_env_values(&[(ALLOW_INSECURE_HTTP_VAR, "1")]);

        assert_eq!(
            Config::from_sources(vars(&all), &dot_env, None).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn a_loopback_http_url_stays_rejected_without_the_flag() {
        let mut all = ALL.to_vec();
        all[0] = (BASE_URL_VAR, "http://localhost:8080");
        assert_eq!(
            Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap_err(),
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

        let error = Config::from_sources(vars(&all), &DotEnv::Absent, None).unwrap_err();

        assert_eq!(error, ConfigError::InvalidBaseUrl(BASE_URL_VAR));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn debug_output_never_contains_credentials() {
        let config = Config::from_sources(vars(ALL), &DotEnv::Absent, None).unwrap();
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
            parse_config_file("jcli_allow_insecure_http = \"1\"\n").unwrap_err(),
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
        let config = Config::from_sources(vars(ALL), &dot_env, Some(&file)).unwrap();
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

        let config = Config::from_sources(absent, &dot_env, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://dotenv.example.com/");
        assert_eq!(config.email, "dotenv@example.com");
        assert_eq!(config.api_token, "dotenv-token");
    }

    #[test]
    fn the_file_supplies_values_only_when_the_dot_env_supplies_none() {
        let file = file_values(FULL_FILE);
        let config = Config::from_sources(vars(&ALL[..1]), &DotEnv::Absent, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://example.atlassian.net/");
        assert_eq!(config.email, "file@example.com");
        assert_eq!(config.api_token, "file-token");
    }

    #[test]
    fn file_only_configuration() {
        let file = file_values(FULL_FILE);
        let config = Config::from_sources(absent, &DotEnv::Absent, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://file.example.com/");
        assert_eq!(config.email, "file@example.com");
        assert_eq!(config.api_token, "file-token");
    }

    #[test]
    fn a_partial_dot_env_disables_the_file_completely() {
        let file = file_values(FULL_FILE);

        // A base URL selected by the project `.env` must not receive
        // credentials from the per-user file.
        let dot_env = dot_env_values(&[(BASE_URL_VAR, "https://dotenv.example.com")]);
        assert_eq!(
            Config::from_sources(absent, &dot_env, Some(&file)).unwrap_err(),
            ConfigError::Missing(EMAIL_VAR)
        );

        // The same protection applies the other way around: credentials in
        // the `.env` must not attach to a file-selected base URL.
        let dot_env = dot_env_values(&[(EMAIL_VAR, "dotenv@example.com")]);
        assert_eq!(
            Config::from_sources(absent, &dot_env, Some(&file)).unwrap_err(),
            ConfigError::Missing(BASE_URL_VAR)
        );

        // The insecure-transport flag counts as jcli-relevant configuration.
        let dot_env = dot_env_values(&[(ALLOW_INSECURE_HTTP_VAR, "1")]);
        assert_eq!(
            Config::from_sources(absent, &dot_env, Some(&file)).unwrap_err(),
            ConfigError::Missing(BASE_URL_VAR)
        );
        assert_eq!(
            ConfigError::DotEnvInvalid.to_string(),
            "the project .env file is invalid"
        );
    }

    #[test]
    fn a_dot_env_with_only_irrelevant_keys_keeps_the_file_active() {
        let file = file_values(FULL_FILE);
        let dot_env = dot_env_values(&[("SOME_OTHER_TOOL_HOME", "/somewhere")]);

        let config = Config::from_sources(absent, &dot_env, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://file.example.com/");
    }

    #[test]
    fn an_empty_dot_env_file_keeps_the_file_active() {
        let file = file_values(FULL_FILE);
        let dot_env = DotEnv::Values(BTreeMap::new());

        let config = Config::from_sources(absent, &dot_env, Some(&file)).unwrap();
        assert_eq!(config.base_url.as_str(), "https://file.example.com/");
    }

    #[test]
    fn a_blank_dot_env_value_is_authoritative_and_disables_the_file() {
        let file = file_values(FULL_FILE);
        let dot_env = dot_env_values(&[(EMAIL_VAR, "   ")]);
        let all = [
            (BASE_URL_VAR, "https://example.atlassian.net"),
            (API_TOKEN_VAR, "token"),
        ];

        assert_eq!(
            Config::from_sources(vars(&all), &dot_env, Some(&file)).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );
    }

    #[test]
    fn an_invalid_dot_env_disables_the_file_instead_of_reenabling_it() {
        // A present `.env` with a valid relevant assignment followed by
        // malformed input must not fall back to looking absent, and the
        // partially parsed entry must not survive.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".env"),
            "JIRA_EMAIL=partial@example.com\nnot a valid line\n",
        )
        .unwrap();

        assert!(matches!(
            read_dot_env(&dir.path().join(".env")),
            DotEnv::Invalid
        ));

        // With a complete TOML file sitting next to that broken `.env`,
        // the file token is still not selected.
        let file = file_values(FULL_FILE);
        let dot_env = read_dot_env(&dir.path().join(".env"));
        assert_eq!(
            Config::from_sources(absent, &dot_env, Some(&file)).unwrap_err(),
            ConfigError::Missing(BASE_URL_VAR)
        );
    }

    #[test]
    fn an_invalid_utf8_dot_env_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".env"), b"JIRA_EMAIL=partial\xff\xfe\n").unwrap();

        assert!(matches!(
            read_dot_env(&dir.path().join(".env")),
            DotEnv::Invalid
        ));
    }

    #[test]
    fn an_oversized_dot_env_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".env"),
            format!("JIRA_EMAIL={}\n", "x".repeat(64 * 1024)),
        )
        .unwrap();

        assert!(matches!(
            read_dot_env(&dir.path().join(".env")),
            DotEnv::Invalid
        ));
    }

    #[test]
    fn a_value_missing_from_all_sources_is_a_configuration_error() {
        let file = file_values("base_url = \"https://file.example.com\"\n");
        assert_eq!(
            Config::from_sources(absent, &DotEnv::Absent, Some(&file)).unwrap_err(),
            ConfigError::Missing(EMAIL_VAR)
        );
    }

    #[test]
    fn a_blank_file_value_reports_empty_while_an_omitted_key_reports_missing() {
        let file = file_values("base_url = \"https://file.example.com\"\nemail = \"\"\n");
        assert_eq!(
            Config::from_sources(absent, &DotEnv::Absent, Some(&file)).unwrap_err(),
            ConfigError::Empty(EMAIL_VAR)
        );

        let file = file_values("base_url = \"https://file.example.com\"\n");
        assert_eq!(
            Config::from_sources(absent, &DotEnv::Absent, Some(&file)).unwrap_err(),
            ConfigError::Missing(EMAIL_VAR)
        );
    }

    #[test]
    fn file_values_are_validated_like_environment_values() {
        let file = file_values("base_url = \"ftp://example.com\"\n");
        assert_eq!(
            Config::from_sources(absent, &DotEnv::Absent, Some(&file)).unwrap_err(),
            ConfigError::InvalidBaseUrl(BASE_URL_VAR)
        );
    }

    #[test]
    fn the_toml_file_is_skipped_when_the_environment_is_complete() {
        // The environment supplies all required settings, so a malformed
        // TOML file can never be reached and cannot block configuration.
        assert!(!toml_participates(&DotEnv::Absent, true));
        assert!(!toml_participates(&DotEnv::Invalid, true));

        // An incomplete environment keeps the file in play, unless the
        // `.env` policy disables it.
        assert!(toml_participates(&DotEnv::Absent, false));
        assert!(!toml_participates(&DotEnv::Invalid, false));
        assert!(!toml_participates(
            &dot_env_values(&[(EMAIL_VAR, "x@y")]),
            false
        ));
    }

    #[test]
    fn reads_the_exact_dot_env_file_without_touching_ancestors() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(dir.path().join(".env"), "JIRA_EMAIL=root@example.com\n").unwrap();

        assert!(matches!(read_dot_env(&nested.join(".env")), DotEnv::Absent));

        let DotEnv::Values(values) = read_dot_env(&dir.path().join(".env")) else {
            panic!("a readable .env must yield values");
        };
        assert_eq!(
            values.get(EMAIL_VAR).map(String::as_str),
            Some("root@example.com")
        );
    }

    #[test]
    fn a_missing_dot_env_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read_dot_env(&dir.path().join(".env")),
            DotEnv::Absent
        ));
    }

    #[test]
    fn duplicate_dot_env_keys_keep_the_first_assignment() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(".env"),
            "JIRA_EMAIL=first@example.com\nJIRA_EMAIL=second@example.com\n",
        )
        .unwrap();

        let DotEnv::Values(values) = read_dot_env(&dir.path().join(".env")) else {
            panic!("a readable .env must yield values");
        };
        assert_eq!(
            values.get(EMAIL_VAR).map(String::as_str),
            Some("first@example.com")
        );
    }

    #[test]
    fn a_bom_prefixed_dot_env_is_parsed() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"JIRA_EMAIL=bom@example.com\n");
        assert!(matches!(
            parse_dot_env(&bytes),
            DotEnv::Values(values) if values.get(EMAIL_VAR).map(String::as_str) == Some("bom@example.com")
        ));
    }

    #[test]
    fn literal_dollars_in_dot_env_values_and_comments_are_preserved() {
        let parsed = parse_dot_env(
            b"# comment = $ignored\nJIRA_EMAIL='literal$'\nJIRA_API_TOKEN=escaped\\$value # $comment\n",
        );
        let DotEnv::Values(values) = parsed else {
            panic!("literal dollars should not be treated as substitutions");
        };
        assert_eq!(values.get(EMAIL_VAR).map(String::as_str), Some("literal$"));
        assert_eq!(
            values.get(API_TOKEN_VAR).map(String::as_str),
            Some("escaped$value")
        );
    }

    #[test]
    fn dot_env_variable_substitution_is_rejected_before_expansion() {
        assert!(matches!(
            parse_dot_env(b"JIRA_EMAIL=${EMAIL}\n"),
            DotEnv::Invalid
        ));
        assert!(matches!(
            parse_dot_env(b"JIRA_EMAIL=\"prefix\n${EMAIL}\"\n"),
            DotEnv::Invalid
        ));
    }

    #[test]
    fn multiline_literal_dollars_and_unicode_comments_are_preserved() {
        let parsed = parse_dot_env(
            "\u{a0}# comment = $ignored\nJIRA_API_TOKEN='prefix\n$literal'\n".as_bytes(),
        );
        let DotEnv::Values(values) = parsed else {
            panic!("literal multiline dollars and comments should be valid");
        };
        assert_eq!(
            values.get(API_TOKEN_VAR).map(String::as_str),
            Some("prefix\n$literal")
        );
    }

    #[test]
    fn dollars_after_unicode_whitespace_in_unquoted_values_are_rejected() {
        assert!(matches!(
            parse_dot_env("JIRA_EMAIL=user@example.com\u{a0}$HOME\n".as_bytes()),
            DotEnv::Invalid
        ));
        assert!(matches!(
            parse_dot_env("JIRA_EMAIL=user@example.com\u{a0}#$HOME\n".as_bytes()),
            DotEnv::Invalid
        ));
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

    #[cfg(unix)]
    #[test]
    fn a_file_without_read_permission_is_unreadable() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            // Root bypasses permission bits, so the read would succeed.
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, FULL_FILE).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();

        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::Unreadable
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_file_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.toml");
        fs::write(&target, FULL_FILE).unwrap();
        let link = dir.path().join("config.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        assert_eq!(
            read_config_file_at(&link).unwrap_err(),
            ConfigError::Unreadable
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_config_file_is_unreadable_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);

        // The open is non-blocking and the FIFO fails the regular-file
        // check, so this returns instead of hanging on a missing writer.
        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::Unreadable
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_dot_env_is_invalid_without_blocking() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0);

        assert!(matches!(read_dot_env(&path), DotEnv::Invalid));
    }

    #[test]
    fn an_oversized_file_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_file(&path, &format!("email = \"{}\"\n", "x".repeat(64 * 1024)));

        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::TooLarge
        );
    }

    #[test]
    fn an_oversized_file_with_invalid_utf8_past_the_cap_is_still_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // The invalid bytes sit beyond the 64 KiB cap; the size check on
        // the raw bytes must win over the later UTF-8 classification.
        let mut contents = format!("email = \"{}\"", "x".repeat(64 * 1024)).into_bytes();
        contents.extend_from_slice(b"\xff\"\n");
        fs::write(&path, contents).unwrap();

        assert_eq!(
            read_config_file_at(&path).unwrap_err(),
            ConfigError::TooLarge
        );
    }

    #[test]
    fn a_file_just_under_the_cap_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let contents = format!("email = \"{}\"\n", "x".repeat(64 * 1024 - 16));
        write_file(&path, &contents);
        assert!(contents.len() <= 64 * 1024);

        let file = read_config_file_at(&path).unwrap().unwrap();
        assert!(file.email.is_some());
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

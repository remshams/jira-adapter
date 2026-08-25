use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// A canonical Jira Cloud issue key, such as `ABC-123`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueKey(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("invalid Jira issue key: expected PROJECT-NUMBER")]
pub struct IssueKeyParseError;

impl IssueKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for IssueKey {
    type Err = IssueKeyParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Jira issue keys use the ASCII form PROJECT-NUMBER. PROJECT is 2-10
        // uppercase letters or digits and starts with a letter. NUMBER is a
        // positive integer without leading zeroes. Bytes make those ASCII-only
        // length and character checks explicit without allocating.
        let bytes = value.as_bytes();
        let separator = bytes
            .iter()
            .position(|&byte| byte == b'-')
            .ok_or(IssueKeyParseError)?;
        let project = &bytes[..separator];
        let number = &bytes[separator + 1..];

        if !(2..=10).contains(&project.len())
            || !project.first().is_some_and(u8::is_ascii_uppercase)
            || !project[1..]
                .iter()
                .all(|&byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
            || number.is_empty()
            || number[0] == b'0'
            || !number.iter().all(u8::is_ascii_digit)
        {
            return Err(IssueKeyParseError);
        }

        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Result<IssueKey, IssueKeyParseError> {
        value.parse()
    }

    #[test]
    fn accepts_canonical_keys() {
        for value in ["AB-1", "ABC-123", "A1B2-987654", "ABCDEFGHIJ-42"] {
            let key = parse(value).unwrap();
            assert_eq!(key.as_str(), value);
            assert_eq!(key.to_string(), value);
        }
    }

    #[test]
    fn accepts_project_length_boundaries() {
        assert!(parse("AB-1").is_ok());
        assert!(parse("ABCDEFGHIJ-1").is_ok());
    }

    #[test]
    fn rejects_lowercase() {
        assert!(parse("Ab-1").is_err());
        assert!(parse("AB-a").is_err());
    }

    #[test]
    fn rejects_whitespace() {
        for value in [" AB-1", "AB-1 ", "A B-1", "AB -1", "AB- 1"] {
            assert!(parse(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn rejects_urls() {
        assert!(parse("https://example.atlassian.net/browse/AB-1").is_err());
        assert!(parse("/browse/AB-1").is_err());
    }

    #[test]
    fn rejects_bare_numbers() {
        assert!(parse("1").is_err());
        assert!(parse("123").is_err());
    }

    #[test]
    fn rejects_underscores() {
        assert!(parse("AB_C-1").is_err());
        assert!(parse("AB-_1").is_err());
    }

    #[test]
    fn rejects_extra_separators() {
        assert!(parse("A-B-1").is_err());
        assert!(parse("AB--1").is_err());
        assert!(parse("AB-1-2").is_err());
    }

    #[test]
    fn rejects_zero_and_leading_zero_issue_numbers() {
        assert!(parse("AB-0").is_err());
        assert!(parse("AB-01").is_err());
    }

    #[test]
    fn rejects_invalid_project_lengths() {
        assert!(parse("A-1").is_err());
        assert!(parse("ABCDEFGHIJK-1").is_err());
    }

    #[test]
    fn does_not_include_input_in_the_error_message() {
        let input = "AB-01";
        let error = parse(input).unwrap_err();
        assert!(!error.to_string().contains(input));
    }
}

use serde::Serialize;

/// Bidi formatting code points that can spoof terminal output. They are
/// not control characters, so serde_json leaves them literal.
pub(crate) const BIDI_CODE_POINTS: &[char] = &[
    '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

/// Renders `value` as stable pretty JSON with one trailing newline.
/// serde_json escapes C0 controls below U+0020 but leaves DEL and the C1
/// range (for example U+009B CSI and U+009D OSC) literal; those never occur
/// as JSON structure, so escaping them cannot break the document. Bidi
/// formatting code points are escaped the same way. Parsing the output
/// recovers the original value.
pub(crate) fn to_pretty_json<T: Serialize>(value: &T) -> String {
    let json = serde_json::to_string_pretty(value).expect("output should serialize");
    let mut rendered = String::with_capacity(json.len());
    for character in json.chars() {
        let is_c1_or_del = ('\u{7F}'..='\u{9F}').contains(&character);
        if is_c1_or_del || BIDI_CODE_POINTS.contains(&character) {
            rendered.push_str(&format!("\\u{:04X}", character as u32));
        } else {
            rendered.push(character);
        }
    }
    rendered.push('\n');
    rendered
}

/// Reduces untrusted text to one terminal-safe line: CR, LF, CRLF, tabs,
/// and the Unicode line and paragraph separators (U+2028, U+2029) become
/// single spaces, while other control characters and bidi code points are
/// dropped. Returns `fallback` when the value is missing or
/// nothing visible remains.
pub(crate) fn terminal_field(value: Option<&str>, fallback: &str) -> String {
    let sanitized = value.map(sanitize_terminal_text).unwrap_or_default();
    if sanitized.trim().is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

/// Folds line breaks, tabs, and the Unicode line and paragraph separators
/// into spaces and drops control and bidirectional formatting characters.
fn sanitize_terminal_text(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.replace("\r\n", "\n").chars() {
        match character {
            '\r' | '\n' | '\t' | '\u{2028}' | '\u{2029}' => sanitized.push(' '),
            character if is_unsafe_terminal_character(character) => {}
            character => sanitized.push(character),
        }
    }
    sanitized
}

fn is_unsafe_terminal_character(character: char) -> bool {
    character.is_control() || BIDI_CODE_POINTS.contains(&character)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pretty_json_ends_with_one_newline() {
        assert_eq!(to_pretty_json(&json!({"a": 1})), "{\n  \"a\": 1\n}\n");
    }

    #[test]
    fn pretty_json_escapes_controls_and_bidi_but_round_trips() {
        let text = "a\u{7F}\u{80}\u{9B}\u{9F}\u{A0}\u{202E}\u{2069}\u{061C}z";
        let rendered = to_pretty_json(&json!({"text": text}));

        for raw in ['\u{7F}', '\u{80}', '\u{9B}', '\u{9F}'] {
            assert!(!rendered.contains(raw), "raw control {raw:?} leaked");
        }
        for bidi in BIDI_CODE_POINTS {
            assert!(!rendered.contains(*bidi), "raw bidi char {bidi:?} leaked");
        }
        assert!(rendered.contains("\\u007F\\u0080\\u009B\\u009F\u{A0}\\u202E\\u2069\\u061Cz"));

        let parsed: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        assert_eq!(parsed["text"], text);
    }

    #[test]
    fn terminal_fields_are_single_line_and_control_free() {
        assert_eq!(
            terminal_field(Some("Jane\r\nDoe\t\u{1b}[31m\u{202e}"), "fallback"),
            "Jane Doe [31m"
        );
        assert_eq!(terminal_field(Some("\u{1b}\u{7}"), "fallback"), "fallback");
        assert_eq!(terminal_field(None, "fallback"), "fallback");
    }

    #[test]
    fn terminal_fields_drop_every_bidi_code_point_and_c1_control() {
        let value: String = BIDI_CODE_POINTS
            .iter()
            .chain(['\u{9b}', '\u{7f}'].iter())
            .collect();
        assert_eq!(
            terminal_field(Some(&format!("a{value}b")), "fallback"),
            "ab"
        );
        assert_eq!(terminal_field(Some("a\rb"), "fallback"), "a b");
    }

    #[test]
    fn terminal_fields_turn_unicode_line_separators_into_spaces() {
        assert_eq!(
            terminal_field(Some("bad\u{2028}error: forged\u{2029}x"), "fallback"),
            "bad error: forged x"
        );
        assert_eq!(
            terminal_field(Some("\u{2028}\u{2029}"), "fallback"),
            "fallback"
        );
    }
}

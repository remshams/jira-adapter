use serde_json::Value;

use crate::jira::issue::{Account, AdfDocument, AdfMark, AdfNode, Issue};
use crate::jira::link::{IssueLink, RemoteLink};

/// Renders a complete Jira issue as stable Markdown, including its final
/// newline. The sections follow each other in a fixed order, separated by one
/// blank line.
pub fn render_issue(issue: &Issue, remote_links: &[RemoteLink]) -> String {
    let sections = [
        render_overview(issue),
        render_description(issue),
        render_links(&issue.fields.issue_links, remote_links),
    ];
    format!("{}\n", sections.join("\n\n"))
}

/// The title line and the metadata list.
fn render_overview(issue: &Issue) -> String {
    let key = escape_metadata(&issue.key);
    let summary = escape_metadata(&issue.fields.summary);
    let issue_type = issue
        .fields
        .issue_type
        .as_ref()
        .and_then(|issue_type| issue_type.name.as_deref())
        .and_then(nonempty_metadata)
        .unwrap_or_else(|| "Unknown".to_string());
    let status = escape_metadata(&issue.fields.status.name);
    let assignee = account_name(issue.fields.assignee.as_ref(), "Unassigned");
    let reporter = account_name(issue.fields.reporter.as_ref(), "Unknown");
    let created = issue
        .fields
        .created
        .as_deref()
        .and_then(timestamp_metadata)
        .unwrap_or_else(|| "Unknown".to_string());
    let labels = issue
        .fields
        .labels
        .iter()
        .filter_map(|label| nonempty_metadata(label))
        .collect::<Vec<_>>();
    let labels = if labels.is_empty() {
        "None".to_string()
    } else {
        labels.join(", ")
    };
    let attachment_count = issue.fields.attachment.len();

    format!(
        "# {key}: {summary}\n\n- Type: {issue_type}\n- Status: {status}\n- Assignee: {assignee}\n- Reporter: {reporter}\n- Created: {created}\n- Labels: {labels}\n- Attachments: {attachment_count}"
    )
}

fn render_description(issue: &Issue) -> String {
    let description = issue
        .fields
        .description
        .as_ref()
        .filter(|document| has_meaningful_content(&document.content))
        .map(render_document)
        .filter(|rendered| !rendered.trim().is_empty())
        .unwrap_or_else(|| "No description.".to_string());
    format!("## Description\n\n{}", description.trim_matches('\n'))
}

/// One list with the issue links, then the remote links, each in Jira's
/// order, or `None` when the issue has no links.
fn render_links(issue_links: &[IssueLink], remote_links: &[RemoteLink]) -> String {
    let items = issue_links
        .iter()
        .map(render_issue_link)
        .chain(remote_links.iter().map(render_remote_link))
        .collect::<Vec<_>>();
    let list = if items.is_empty() {
        "None".to_string()
    } else {
        items.join("\n")
    };
    format!("## Links\n\n{list}")
}

fn render_issue_link(link: &IssueLink) -> String {
    format!(
        "- {} {}: {} ({})",
        metadata_or_unknown(link.relation.as_deref()),
        metadata_or_unknown(link.key.as_deref()),
        metadata_or_unknown(link.summary.as_deref()),
        metadata_or_unknown(link.status.as_deref())
    )
}

/// Shows the URL as an autolink only when it is an absolute `http` or
/// `https` URL, the rule ADF links follow; otherwise only the title.
fn render_remote_link(link: &RemoteLink) -> String {
    let title = metadata_or_unknown(link.title.as_deref());
    match link.url.as_deref().filter(|url| is_safe_link(url)) {
        Some(url) => format!("- Remote: {title} <{}>", escape_link_destination(url)),
        None => format!("- Remote: {title}"),
    }
}

fn metadata_or_unknown(value: Option<&str>) -> String {
    value
        .and_then(nonempty_metadata)
        .unwrap_or_else(|| "Unknown".to_string())
}

fn account_name(account: Option<&Account>, missing: &str) -> String {
    match account {
        None => missing.to_string(),
        Some(account) => account
            .display_name
            .as_deref()
            .and_then(nonempty_metadata)
            .unwrap_or_else(|| "Unknown".to_string()),
    }
}

fn render_document(document: &AdfDocument) -> String {
    render_blocks(&document.content)
}

fn render_blocks(nodes: &[AdfNode]) -> String {
    render_mixed_content(String::new(), nodes)
}

fn render_mixed_content(mut inline: String, nodes: &[AdfNode]) -> String {
    let mut blocks = Vec::new();

    for node in nodes {
        if is_block_node(node) {
            push_nonempty(
                &mut blocks,
                protect_inline_block_starts(&std::mem::take(&mut inline)),
            );
            push_nonempty(&mut blocks, render_block(node));
        } else {
            inline.push_str(&render_inline_node(node));
        }
    }
    push_nonempty(&mut blocks, protect_inline_block_starts(&inline));

    blocks.join("\n\n")
}

fn is_block_node(node: &AdfNode) -> bool {
    matches!(
        node.node_type.as_str(),
        "doc"
            | "document"
            | "paragraph"
            | "heading"
            | "bulletList"
            | "orderedList"
            | "listItem"
            | "codeBlock"
            | "blockquote"
    ) || node.content.iter().any(is_block_node)
}

fn push_nonempty(blocks: &mut Vec<String>, block: String) {
    if !block.trim().is_empty() {
        blocks.push(block.trim_matches('\n').to_string());
    }
}

fn render_block(node: &AdfNode) -> String {
    match node.node_type.as_str() {
        "bulletList" | "orderedList" | "listItem" => render_list_block(node),
        "heading" => render_heading(node),
        "codeBlock" => render_code_block(node),
        "blockquote" => render_blockquote(node),
        _ => render_unknown_block(node),
    }
}

fn render_list_block(node: &AdfNode) -> String {
    match node.node_type.as_str() {
        "bulletList" => render_list(node, None),
        "orderedList" => render_list(
            node,
            Some(ordered_list_start(node.attrs.as_ref()).unwrap_or(1)),
        ),
        _ => render_list_item_contents(&node.content),
    }
}

fn render_heading(node: &AdfNode) -> String {
    let level = integer_attr(node.attrs.as_ref(), "level").filter(|level| (1..=6).contains(level));
    let Some(first_block) = node.content.iter().position(is_block_node) else {
        let content = protect_inline_block_starts(&format!(
            "{}{}",
            render_own_text(node),
            render_inlines(&node.content)
        ));
        return match level {
            Some(level) => format!("{} {content}", "#".repeat(level as usize)),
            None => content,
        };
    };

    let leading = protect_inline_block_starts(&format!(
        "{}{}",
        render_own_text(node),
        render_inlines(&node.content[..first_block])
    ));
    let leading = match level {
        Some(level) if !leading.trim().is_empty() => {
            format!("{} {leading}", "#".repeat(level as usize))
        }
        _ => leading,
    };

    let mut blocks = Vec::new();
    push_nonempty(&mut blocks, leading);
    push_nonempty(&mut blocks, render_blocks(&node.content[first_block..]));
    blocks.join("\n\n")
}

const MAX_ORDERED_LIST_MARKER: u64 = 999_999_999;

fn render_list(node: &AdfNode, ordered_start: Option<u64>) -> String {
    let mut items = Vec::new();
    let mut number = ordered_start.unwrap_or(1);

    for child in &node.content {
        let content = render_block(child);
        if content.trim().is_empty() {
            continue;
        }

        let prefix = if ordered_start.is_some() {
            let prefix = format!("{number}. ");
            number = if number == MAX_ORDERED_LIST_MARKER {
                1
            } else {
                number + 1
            };
            prefix
        } else {
            "- ".to_string()
        };
        items.push(prefix_multiline(&prefix, &content));
    }

    items.join("\n")
}

fn render_list_item_contents(nodes: &[AdfNode]) -> String {
    join_list_parts(&list_item_parts(nodes))
}

/// Renders list item children into non-empty parts, each flagged with
/// whether it is a nested list.
fn list_item_parts(nodes: &[AdfNode]) -> Vec<(String, bool)> {
    let mut parts = Vec::new();
    let mut inline = String::new();
    for node in nodes {
        if is_block_node(node) {
            push_list_part(
                &mut parts,
                protect_inline_block_starts(&std::mem::take(&mut inline)),
                false,
            );
            push_list_part(
                &mut parts,
                render_block(node),
                matches!(node.node_type.as_str(), "bulletList" | "orderedList"),
            );
        } else {
            inline.push_str(&render_inline_node(node));
        }
    }
    push_list_part(&mut parts, protect_inline_block_starts(&inline), false);
    parts
}

/// Joins list item parts with a blank line between paragraphs and a single
/// newline next to nested lists.
fn join_list_parts(parts: &[(String, bool)]) -> String {
    let Some(((first, first_is_list), remaining)) = parts.split_first() else {
        return String::new();
    };
    let mut output = first.clone();
    let mut previous_is_list = *first_is_list;
    for (part, is_list) in remaining {
        output.push_str(if *is_list || previous_is_list {
            "\n"
        } else {
            "\n\n"
        });
        output.push_str(part);
        previous_is_list = *is_list;
    }
    output
}

fn push_list_part(parts: &mut Vec<(String, bool)>, rendered: String, is_list: bool) {
    if !rendered.trim().is_empty() {
        parts.push((rendered, is_list));
    }
}

fn prefix_multiline(prefix: &str, content: &str) -> String {
    let indent = " ".repeat(prefix.chars().count());
    let mut lines = content.lines();
    let first = lines.next().unwrap_or_default();
    let mut output = format!("{prefix}{first}");
    for line in lines {
        output.push('\n');
        if !line.is_empty() {
            output.push_str(&indent);
            output.push_str(line);
        }
    }
    output
}

fn render_code_block(node: &AdfNode) -> String {
    let content = sanitize_code(&collect_raw_text(node));
    let fence = "`".repeat(longest_backtick_run(&content).saturating_add(1).max(3));
    let language = string_attr(node.attrs.as_ref(), "language")
        .filter(|language| is_safe_language(language))
        .unwrap_or_default();
    let newline = if content.ends_with('\n') { "" } else { "\n" };
    format!("{fence}{language}\n{content}{newline}{fence}")
}

fn render_blockquote(node: &AdfNode) -> String {
    render_blocks(&node.content)
        .lines()
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_unknown_block(node: &AdfNode) -> String {
    render_mixed_content(render_own_text(node), &node.content)
}

fn render_own_text(node: &AdfNode) -> String {
    node.text
        .as_deref()
        .map(|text| render_marked_text(text, &node.marks))
        .unwrap_or_default()
}

fn render_inlines(nodes: &[AdfNode]) -> String {
    nodes.iter().map(render_inline_node).collect()
}

fn render_inline_node(node: &AdfNode) -> String {
    match node.node_type.as_str() {
        "hardBreak" => "  \n".to_string(),
        _ => {
            let mut rendered = node
                .text
                .as_deref()
                .map(|text| render_marked_text(text, &node.marks))
                .unwrap_or_default();
            rendered.push_str(&render_inlines(&node.content));
            rendered
        }
    }
}

fn render_marked_text(text: &str, marks: &[AdfMark]) -> String {
    let content = sanitize_inline(text);
    let has_code = marks.iter().any(|mark| mark.mark_type == "code");
    let rendered = if has_code {
        render_inline_code(&content)
    } else {
        escape_markdown(&content, true)
    };
    marks.iter().fold(rendered, apply_mark)
}

fn apply_mark(rendered: String, mark: &AdfMark) -> String {
    match mark.mark_type.as_str() {
        "strong" => wrap_emphasis(rendered, "**"),
        "em" => wrap_emphasis(rendered, "*"),
        "link" => wrap_link(rendered, mark),
        _ => rendered,
    }
}

fn wrap_link(rendered: String, mark: &AdfMark) -> String {
    match link_destination(mark.attrs.as_ref()) {
        Some(destination) => format!("[{rendered}]({destination})"),
        None => rendered,
    }
}

fn wrap_emphasis(rendered: String, delimiter: &str) -> String {
    let without_leading = rendered.trim_start_matches(char::is_whitespace);
    let leading_len = rendered.len() - without_leading.len();
    let core = without_leading.trim_end_matches(char::is_whitespace);
    if core.is_empty() {
        return rendered;
    }
    let leading = &rendered[..leading_len];
    let trailing = &without_leading[core.len()..];
    format!("{leading}{delimiter}{core}{delimiter}{trailing}")
}

fn render_inline_code(content: &str) -> String {
    let fence = "`".repeat(longest_backtick_run(content).saturating_add(1).max(1));
    let needs_padding = content.starts_with('`')
        || content.ends_with('`')
        || (content.starts_with(' ')
            && content.ends_with(' ')
            && content.chars().any(|character| character != ' '));
    if needs_padding {
        format!("{fence} {content} {fence}")
    } else {
        format!("{fence}{content}{fence}")
    }
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

fn ordered_list_start(attrs: Option<&Value>) -> Option<u64> {
    integer_attr(attrs, "order").filter(|order| (1..=MAX_ORDERED_LIST_MARKER).contains(order))
}

fn integer_attr(attrs: Option<&Value>, name: &str) -> Option<u64> {
    attrs?.as_object()?.get(name)?.as_u64()
}

fn string_attr<'a>(attrs: Option<&'a Value>, name: &str) -> Option<&'a str> {
    attrs?.as_object()?.get(name)?.as_str()
}

fn is_safe_language(language: &str) -> bool {
    !language.is_empty()
        && language.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '+' | '-' | '#' | '.')
        })
}

fn link_destination(attrs: Option<&Value>) -> Option<String> {
    let href = string_attr(attrs, "href")?;
    is_safe_link(href).then(|| escape_link_destination(href))
}

fn is_safe_link(href: &str) -> bool {
    !href.trim().is_empty()
        && !href.chars().any(is_unsafe_control)
        && url::Url::parse(href).is_ok_and(|parsed| is_web_url(&parsed, href))
}

/// Accepts absolute http(s) URLs with a host, written with `://` exactly as
/// parsed, so the rendered link points where the raw text says it does.
fn is_web_url(parsed: &url::Url, href: &str) -> bool {
    let scheme = parsed.scheme();
    href.get(scheme.len()..scheme.len() + 3) == Some("://")
        && parsed.has_host()
        && (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
}

/// Characters that would end or break a Markdown link destination, with
/// their percent-encoded replacements.
const LINK_ESCAPES: [(char, &str); 7] = [
    ('\\', "%5C"),
    (' ', "%20"),
    ('(', "%28"),
    (')', "%29"),
    ('<', "%3C"),
    ('>', "%3E"),
    ('"', "%22"),
];

fn escape_link_destination(href: &str) -> String {
    let mut escaped = String::new();
    for character in href.chars() {
        match LINK_ESCAPES
            .iter()
            .find(|(unsafe_character, _)| *unsafe_character == character)
        {
            Some((_, replacement)) => escaped.push_str(replacement),
            None => escaped.push(character),
        }
    }
    escaped
}

fn escape_metadata(text: &str) -> String {
    let sanitized = sanitize_single_line(text);
    escape_markdown(&sanitized, false)
}

fn nonempty_metadata(text: &str) -> Option<String> {
    let sanitized = sanitize_single_line(text);
    if sanitized.trim().is_empty() {
        None
    } else {
        Some(escape_markdown(&sanitized, false))
    }
}

/// Renders a Jira timestamp as returned. Well-formed timestamps hold only
/// digits and `-:.+TZ`, none of which can start Markdown structure in the
/// middle of a line, so they skip escaping that would turn `+0200` into
/// `\+0200`. Any other value goes through the usual metadata escaping.
fn timestamp_metadata(text: &str) -> Option<String> {
    let is_plain_timestamp = !text.is_empty()
        && text
            .chars()
            .all(|character| character.is_ascii_digit() || "-:.+TZ".contains(character));
    if is_plain_timestamp {
        Some(text.to_string())
    } else {
        nonempty_metadata(text)
    }
}

fn escape_markdown(text: &str, escape_hyphen: bool) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '<'
                | '>'
                | '#'
                | '+'
                | '!'
                | '|'
                | '~'
                | '='
        ) || (escape_hyphen && character == '-')
        {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn protect_inline_block_starts(text: &str) -> String {
    protect_indented_code_starts(&protect_ordered_list_starts(text))
}

fn protect_indented_code_starts(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            if line.starts_with("    ") {
                format!("&#32;{}", &line[1..])
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn protect_ordered_list_starts(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let content = line.strip_suffix('\n').unwrap_or(line);
            let newline = if line.ends_with('\n') { "\n" } else { "" };
            let indentation = content.len() - content.trim_start_matches(' ').len();
            let rest = &content[indentation..];
            let digit_count = rest.bytes().take_while(u8::is_ascii_digit).count();
            let delimiter = rest.as_bytes().get(digit_count);
            let following = rest.as_bytes().get(digit_count + 1);
            if indentation <= 3
                && digit_count > 0
                && delimiter == Some(&b'.')
                && following.is_some_and(u8::is_ascii_whitespace)
            {
                format!(
                    "{}{}\\.{}{}",
                    &content[..indentation],
                    &rest[..digit_count],
                    &rest[digit_count + 1..],
                    newline
                )
            } else {
                line.to_string()
            }
        })
        .collect()
}

fn sanitize_single_line(text: &str) -> String {
    let mut sanitized = String::new();
    for character in text.chars() {
        if matches!(character, '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}') {
            sanitized.push(' ');
        } else if !is_unsafe_control(character) {
            sanitized.push(character);
        }
    }
    sanitized
}

fn sanitize_inline(text: &str) -> String {
    let mut sanitized = String::new();
    for character in normalize_line_endings(text).chars() {
        match character {
            '\n' | '\t' | '\u{2028}' | '\u{2029}' => sanitized.push(' '),
            character if is_unsafe_control(character) => {}
            character => sanitized.push(character),
        }
    }
    sanitized
}

fn sanitize_code(text: &str) -> String {
    normalize_line_endings(text)
        .chars()
        .filter(|&character| is_code_character(character))
        .collect()
}

fn is_code_character(character: char) -> bool {
    matches!(character, '\n' | '\t') || !is_unsafe_control(character)
}

/// Folds `\r\n` pairs and lone `\r` into a single `\n`.
fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn is_unsafe_control(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}

fn collect_raw_text(node: &AdfNode) -> String {
    let mut text = String::new();
    append_raw_text(node, &mut text);
    text
}

fn append_raw_text(node: &AdfNode, text: &mut String) {
    if let Some(own_text) = &node.text {
        text.push_str(own_text);
    }
    for child in &node.content {
        if child.node_type == "hardBreak" {
            text.push('\n');
        } else {
            append_raw_text(child, text);
        }
    }
}

fn has_meaningful_content(nodes: &[AdfNode]) -> bool {
    nodes.iter().any(|node| {
        node.text
            .as_deref()
            .map(sanitize_code)
            .is_some_and(|text| text.chars().any(|character| !character.is_whitespace()))
            || has_meaningful_content(&node.content)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(content: Value) -> AdfDocument {
        serde_json::from_value(json!({
            "version": 1,
            "type": "doc",
            "content": content
        }))
        .unwrap()
    }

    fn render(content: Value) -> String {
        render_document(&document(content))
    }

    fn issue(description: Option<AdfDocument>) -> Issue {
        use crate::jira::issue::{IssueFields, IssueType, Status};

        Issue {
            key: "ABC-123".to_string(),
            fields: IssueFields {
                summary: "Fix the thing".to_string(),
                issue_type: Some(IssueType {
                    name: Some("Bug".to_string()),
                }),
                status: Status {
                    name: "In Progress".to_string(),
                },
                assignee: None,
                reporter: Some(Account {
                    account_id: "reporter-id".to_string(),
                    display_name: Some("Jane Doe".to_string()),
                    email_address: Some("jane@example.com".to_string()),
                }),
                labels: vec!["backend".to_string(), "urgent".to_string()],
                created: Some("2026-10-17T02:13:45.123+0200".to_string()),
                description,
                attachment: Vec::new(),
                issue_links: Vec::new(),
            },
        }
    }

    #[test]
    fn renders_paragraph_heading_and_hard_break_blocks() {
        assert_eq!(
            render(json!([
                {"type": "heading", "attrs": {"level": 3}, "content": [{"type": "text", "text": "Title"}]},
                {"type": "paragraph", "content": [
                    {"type": "text", "text": "first"},
                    {"type": "hardBreak"},
                    {"type": "text", "text": "second"}
                ]}
            ])),
            "### Title\n\nfirst  \nsecond"
        );
    }

    #[test]
    fn renders_document_alias_and_all_heading_levels() {
        let content = (1..=6)
            .map(|level| {
                json!({
                    "type": "heading",
                    "attrs": {"level": level},
                    "content": [{"type": "text", "text": "Title"}]
                })
            })
            .collect::<Vec<_>>();
        let document: AdfDocument = serde_json::from_value(json!({
            "version": 1,
            "type": "document",
            "content": content
        }))
        .unwrap();

        assert_eq!(
            render_document(&document),
            "# Title\n\n## Title\n\n### Title\n\n#### Title\n\n##### Title\n\n###### Title"
        );
    }

    #[test]
    fn malformed_heading_levels_fall_back_to_plain_text() {
        for attrs in [
            json!(null),
            json!({}),
            json!({"level": 0}),
            json!({"level": 7}),
            json!({"level": "2"}),
        ] {
            assert_eq!(
                render(
                    json!([{"type": "heading", "attrs": attrs, "content": [{"type": "text", "text": "Title"}]}])
                ),
                "Title"
            );
        }
    }

    #[test]
    fn renders_supported_marks_and_preserves_unknown_marks() {
        assert_eq!(
            render(json!([{"type": "paragraph", "content": [
                {"type": "text", "text": "bold", "marks": [{"type": "strong"}]},
                {"type": "text", "text": " italic", "marks": [{"type": "em"}]},
                {"type": "text", "text": " code", "marks": [{"type": "code"}]},
                {"type": "text", "text": " link", "marks": [{"type": "link", "attrs": {"href": "https://example.com/a b(c)\\d"}}]},
                {"type": "text", "text": " future", "marks": [{"type": "future"}]}
            ]}])),
            "**bold** *italic*` code`[ link](https://example.com/a%20b%28c%29%5Cd) future"
        );
    }

    #[test]
    fn emphasis_keeps_surrounding_whitespace_outside_delimiters() {
        for (mark_type, delimiter) in [("strong", "**"), ("em", "*")] {
            let marks = [AdfMark {
                mark_type: mark_type.to_string(),
                attrs: None,
            }];
            assert_eq!(
                render_marked_text(" leading", &marks),
                format!(" {delimiter}leading{delimiter}")
            );
            assert_eq!(
                render_marked_text("trailing ", &marks),
                format!("{delimiter}trailing{delimiter} ")
            );
            assert_eq!(render_marked_text("   ", &marks), "   ");
        }
    }

    #[test]
    fn invalid_link_attrs_render_only_the_label() {
        for attrs in [
            json!(null),
            json!({}),
            json!({"href": 7}),
            json!({"href": ""}),
            json!({"href": "https://example.com/\u{1b}[31m"}),
        ] {
            assert_eq!(
                render(
                    json!([{"type": "paragraph", "content": [{"type": "text", "text": "label", "marks": [{"type": "link", "attrs": attrs}]}]}])
                ),
                "label"
            );
        }
    }

    #[test]
    fn link_destinations_escape_markdown_delimiters() {
        assert_eq!(
            render(json!([{"type": "paragraph", "content": [{
                "type": "text",
                "text": "label",
                "marks": [{"type": "link", "attrs": {
                    "href": "https://example.com/a<b>c\"d"
                }}]
            }]}])),
            "[label](https://example.com/a%3Cb%3Ec%22d)"
        );
    }

    #[test]
    fn only_absolute_http_and_https_links_are_active() {
        for href in [
            "JaVaScRiPt:alert(1)",
            "JaVaScRiPt://example.com/alert",
            "DaTa:text/plain,hello",
            "VbScRiPt://example.com/message",
            "example.com/path",
            "/relative/path",
            "not a URL",
            "https:example.com",
            "http://",
            "https://exa\u{7}mple.com",
        ] {
            assert_eq!(
                render(json!([{"type": "paragraph", "content": [{
                    "type": "text",
                    "text": "label",
                    "marks": [{"type": "link", "attrs": {"href": href}}]
                }]}])),
                "label",
                "href should not produce an active link: {href:?}"
            );
        }

        assert_eq!(
            render(json!([{"type": "paragraph", "content": [
                {"type": "text", "text": "http", "marks": [{"type": "link", "attrs": {"href": "HtTp://example.com/a b(c)"}}]},
                {"type": "text", "text": " https", "marks": [{"type": "link", "attrs": {"href": "https://example.com/path"}}]}
            ]}])),
            "[http](HtTp://example.com/a%20b%28c%29)[ https](https://example.com/path)"
        );
    }

    #[test]
    fn raw_inline_line_endings_do_not_create_markdown_structure() {
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "plain\ntext"}]}])
            ),
            "plain text"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "strong\r\ntext", "marks": [{"type": "strong"}]}]}])
            ),
            "**strong text**"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "em\n\ntext", "marks": [{"type": "em"}]}]}])
            ),
            "*em  text*"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "link\r\rtext", "marks": [{"type": "link", "attrs": {"href": "https://example.com"}}]}]}])
            ),
            "[link  text](https://example.com)"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "code\n\ntext", "marks": [{"type": "code"}]}]}])
            ),
            "`code  text`"
        );
    }

    #[test]
    fn leading_inline_indentation_preserves_text_without_starting_a_code_block() {
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "    plain\n\n    text"}]}])
            ),
            "&#32;   plain      text"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "    strong\n\n    text", "marks": [{"type": "strong"}]}]}])
            ),
            "&#32;   **strong      text**"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "    em\n\n    text", "marks": [{"type": "em"}]}]}])
            ),
            "&#32;   *em      text*"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "    link\n\n    text", "marks": [{"type": "link", "attrs": {"href": "https://example.com"}}]}]}])
            ),
            "[    link      text](https://example.com)"
        );
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "    code\n\n    text", "marks": [{"type": "code"}]}]}])
            ),
            "`    code      text`"
        );
    }

    #[test]
    fn inline_code_uses_a_delimiter_longer_than_its_backticks() {
        for (content, expected) in [
            ("a``b", "```a``b```"),
            ("`edge`", "`` `edge` ``"),
            ("`start", "`` `start ``"),
            ("end`", "`` end` ``"),
            (" spaced ", "`  spaced  `"),
            ("   ", "`   `"),
        ] {
            assert_eq!(
                render(
                    json!([{"type": "paragraph", "content": [{"type": "text", "text": content, "marks": [{"type": "code"}]}]}])
                ),
                expected
            );
        }
    }

    #[test]
    fn renders_nested_bullet_and_ordered_lists_with_multiline_items() {
        assert_eq!(
            render(json!([
                {"type": "bulletList", "content": [
                    {"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "outer"}]},
                        {"type": "orderedList", "attrs": {"order": 3}, "content": [
                            {"type": "listItem", "content": [{"type": "paragraph", "content": [
                                {"type": "text", "text": "inner"}, {"type": "hardBreak"}, {"type": "text", "text": "continued"}
                            ]}]},
                            {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "next"}]}]}
                        ]}
                    ]}
                ]}
            ])),
            "- outer\n  3. inner  \n     continued\n  4. next"
        );
    }

    #[test]
    fn invalid_ordered_list_orders_start_at_one() {
        for attrs in [
            json!(null),
            json!({}),
            json!({"order": 0}),
            json!({"order": -2}),
            json!({"order": "4"}),
            json!({"order": 1_000_000_000_u64}),
            json!({"order": u64::MAX}),
        ] {
            assert_eq!(
                render(
                    json!([{"type": "orderedList", "attrs": attrs, "content": [{"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "item"}]}]}]}])
                ),
                "1. item"
            );
        }
    }

    #[test]
    fn ordered_list_markers_wrap_before_ten_digits_without_repeating() {
        assert_eq!(
            render(
                json!([{"type": "orderedList", "attrs": {"order": 999_999_998_u64}, "content": [
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "first"}]}]},
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "second"}]}]},
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "third"}]}]},
                    {"type": "listItem", "content": [{"type": "paragraph", "content": [{"type": "text", "text": "fourth"}]}]}
                ]}])
            ),
            "999999998. first\n999999999. second\n1. third\n2. fourth"
        );
    }

    #[test]
    fn code_blocks_normalize_line_endings_and_remove_controls() {
        assert_eq!(
            render(
                json!([{"type": "codeBlock", "content": [{"type": "text", "text": "a\r\nb\rc\t\u{1b}"}]}])
            ),
            "```\na\nb\nc\t\n```"
        );
    }

    #[test]
    fn renders_code_blocks_with_safe_languages_and_long_fences() {
        assert_eq!(
            render(
                json!([{"type": "codeBlock", "attrs": {"language": "rust"}, "content": [{"type": "text", "text": "let fence = ````;\n"}]}])
            ),
            "`````rust\nlet fence = ````;\n`````"
        );
        assert_eq!(
            render(
                json!([{"type": "codeBlock", "attrs": {"language": "bad`\n#"}, "content": [{"type": "text", "text": "x"}]}])
            ),
            "```\nx\n```"
        );
    }

    #[test]
    fn blockquotes_prefix_every_line_including_blank_lines() {
        assert_eq!(
            render(json!([{"type": "blockquote", "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "one"}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "two"}]}
            ]}])),
            "> one\n>\n> two"
        );
    }

    #[test]
    fn unknown_nodes_preserve_text_and_known_descendants() {
        assert_eq!(
            render(json!([{"type": "futureBlock", "text": "own", "content": [
                {"type": "paragraph", "content": [
                    {"type": "futureInline", "content": [{"type": "text", "text": "kept", "marks": [{"type": "futureMark"}]}]}
                ]}
            ]}])),
            "own\n\nkept"
        );
    }

    #[test]
    fn paragraph_with_malformed_block_children_keeps_block_separation() {
        assert_eq!(
            render(json!([{"type": "paragraph", "content": [
                {"type": "text", "text": "before"},
                {"type": "bulletList", "content": [{"type": "listItem", "content": [
                    {"type": "paragraph", "content": [{"type": "text", "text": "nested"}]}
                ]}]},
                {"type": "codeBlock", "content": [{"type": "text", "text": "code"}]},
                {"type": "blockquote", "content": [{"type": "paragraph", "content": [
                    {"type": "text", "text": "quote"}
                ]}]},
                {"type": "text", "text": "after"}
            ]}])),
            "before\n\n- nested\n\n```\ncode\n```\n\n> quote\n\nafter"
        );
    }

    #[test]
    fn unknown_inline_wrapper_with_block_descendants_keeps_block_separation() {
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "futureInline", "content": [
                    {"type": "text", "text": "before"},
                    {"type": "orderedList", "attrs": {"order": 3}, "content": [{"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "nested"}]}
                    ]}]},
                    {"type": "codeBlock", "content": [{"type": "text", "text": "code"}]},
                    {"type": "blockquote", "content": [{"type": "paragraph", "content": [
                        {"type": "text", "text": "quote"}
                    ]}]},
                    {"type": "text", "text": "after"}
                ]}]}])
            ),
            "before\n\n3. nested\n\n```\ncode\n```\n\n> quote\n\nafter"
        );
    }

    #[test]
    fn malformed_heading_content_keeps_nested_blocks_separate() {
        assert_eq!(
            render(
                json!([{"type": "heading", "attrs": {"level": 2}, "content": [
                    {"type": "text", "text": "before"},
                    {"type": "bulletList", "content": [{"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "nested"}]}
                    ]}]},
                    {"type": "text", "text": "after"}
                ]}])
            ),
            "## before\n\n- nested\n\nafter"
        );
        assert_eq!(
            render(
                json!([{"type": "heading", "attrs": {"level": 2}, "content": [
                    {"type": "bulletList", "content": [{"type": "listItem", "content": [
                        {"type": "paragraph", "content": [{"type": "text", "text": "nested"}]}
                    ]}]}
                ]}])
            ),
            "- nested"
        );
    }

    #[test]
    fn escapes_markdown_and_removes_terminal_controls() {
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "# [x](y) *z* - q\u{1b}[31m\u{7}"}]}])
            ),
            "\\# \\[x\\]\\(y\\) \\*z\\* \\- q\\[31m"
        );
    }

    #[test]
    fn unicode_line_separators_in_text_become_spaces() {
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "a\u{2028}b\u{2029}c"}]}])
            ),
            "a b c"
        );
    }

    #[test]
    fn ordered_list_lookalikes_remain_plain_text() {
        assert_eq!(
            protect_ordered_list_starts(". item\n123x item\n1. item\n"),
            ". item\n123x item\n1\\. item\n"
        );
    }

    #[test]
    fn plain_text_cannot_change_block_structure() {
        assert_eq!(
            render(
                json!([{"type": "paragraph", "content": [{"type": "text", "text": "1. forged\n42. second\nTitle\n==="}]}])
            ),
            "1\\. forged 42. second Title \\=\\=\\="
        );
        assert_eq!(
            render(json!([{"type": "text", "text": "1. direct"}])),
            "1\\. direct"
        );
        assert_eq!(
            render(
                json!([{"type": "heading", "attrs": {}, "content": [{"type": "text", "text": "1. malformed"}]}])
            ),
            "1\\. malformed"
        );
    }

    #[test]
    fn missing_and_effectively_empty_descriptions_use_the_fallback() {
        assert!(
            render_issue(&issue(None), &[])
                .ends_with("## Description\n\nNo description.\n\n## Links\n\nNone\n")
        );

        let empty_content = [
            json!([{"type": "paragraph", "content": [{"type": "text", "text": " \n\t\u{1b}"}]}]),
            json!([{"type": "codeBlock", "content": []}]),
            json!([{"type": "codeBlock", "content": [{"type": "text", "text": " \n\t\u{1b}"}]}]),
            json!([{"type": "paragraph", "content": [{
                "type": "text",
                "text": " \t",
                "marks": [{"type": "strong"}]
            }]}]),
            json!([{"type": "paragraph", "content": [{
                "type": "text",
                "text": " \t",
                "marks": [{"type": "em"}, {"type": "code"}, {
                    "type": "link",
                    "attrs": {"href": "https://example.com"}
                }]
            }]}]),
        ];
        for content in empty_content {
            let output = render_issue(&issue(Some(document(content))), &[]);
            assert!(output.ends_with("## Description\n\nNo description.\n\n## Links\n\nNone\n"));
        }
    }

    #[test]
    fn metadata_fallbacks_do_not_render_private_account_fields() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open"},
                "assignee": {"accountId": "secret-id", "emailAddress": "secret@example.com"},
                "reporter": null,
                "labels": [],
                "description": null
            }
        }))
        .unwrap();
        let output = render_issue(&issue, &[]);

        assert!(output.contains(
            "- Type: Unknown\n- Status: Open\n- Assignee: Unknown\n- Reporter: Unknown\n- Created: Unknown\n- Labels: None\n- Attachments: 0\n"
        ));
        assert!(!output.contains("secret-id"));
        assert!(!output.contains("secret@example.com"));
    }

    #[test]
    fn issue_attachment_metadata_renders_only_the_count() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open"},
                "assignee": null,
                "reporter": null,
                "labels": [],
                "description": null,
                "attachment": [{"id": "private-id", "filename": "private.txt", "size": 1}]
            }
        }))
        .unwrap();

        let output = render_issue(&issue, &[]);

        assert!(output.contains("- Attachments: 1\n"));
        assert!(!output.contains("private-id"));
        assert!(!output.contains("private.txt"));
    }

    #[test]
    fn metadata_cannot_change_markdown_structure_or_control_the_terminal() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123\n# forged",
            "fields": {
                "summary": "*summary*\u{1b}[31m",
                "status": {"name": "> Open"},
                "assignee": null,
                "reporter": {"accountId": "id", "displayName": "[Jane](bad)"},
                "labels": ["# label", "a|b"],
                "description": null
            }
        }))
        .unwrap();
        let output = render_issue(&issue, &[]);

        assert!(output.starts_with("# ABC-123 \\# forged: \\*summary\\*\\[31m\n"));
        assert!(output.contains("- Status: \\> Open\n"));
        assert!(output.contains("- Reporter: \\[Jane\\]\\(bad\\)\n"));
        assert!(output.contains("- Labels: \\# label, a\\|b\n"));
        assert!(!output.contains('\u{1b}'));
    }

    #[test]
    fn account_and_label_fallbacks_use_sanitized_values() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open"},
                "assignee": {"accountId": "id", "displayName": "\u{1b}\u{7}"},
                "reporter": {"accountId": "id", "displayName": "\r\n\t"},
                "labels": ["\u{1b}", "\r\n", "ke\u{7}pt", "\u{7}"],
                "description": null
            }
        }))
        .unwrap();
        let output = render_issue(&issue, &[]);

        assert!(output.contains(
            "- Assignee: Unknown\n- Reporter: Unknown\n- Created: Unknown\n- Labels: kept\n- Attachments: 0\n"
        ));
    }

    #[test]
    fn labels_fall_back_to_none_when_all_sanitized_values_are_empty() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "status": {"name": "Open"},
                "assignee": null,
                "reporter": null,
                "labels": ["\u{1b}", "\u{7}", "\r\n\t"],
                "description": null
            }
        }))
        .unwrap();

        assert!(render_issue(&issue, &[]).contains("- Labels: None\n- Attachments: 0\n"));
    }

    #[test]
    fn renders_the_exact_complete_issue_contract() {
        let description = document(
            json!([{"type": "paragraph", "content": [{"type": "text", "text": "Rendered description."}]}]),
        );

        assert_eq!(
            render_issue(&issue(Some(description)), &[]),
            "# ABC-123: Fix the thing\n\n- Type: Bug\n- Status: In Progress\n- Assignee: Unassigned\n- Reporter: Jane Doe\n- Created: 2026-10-17T02:13:45.123+0200\n- Labels: backend, urgent\n- Attachments: 0\n\n## Description\n\nRendered description.\n\n## Links\n\nNone\n"
        );
    }

    #[test]
    fn issue_type_and_created_fall_back_when_missing_or_empty() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "issuetype": {"name": "\u{1b}\r\n"},
                "status": {"name": "Open"},
                "created": "",
                "description": null
            }
        }))
        .unwrap();

        assert!(render_issue(&issue, &[]).contains("- Type: Unknown\n"));
        assert!(render_issue(&issue, &[]).contains("- Created: Unknown\n"));
    }

    #[test]
    fn issue_types_without_a_name_render_as_unknown() {
        for issue_type in [json!({}), json!({"name": null})] {
            let issue: Issue = serde_json::from_value(json!({
                "key": "ABC-123",
                "fields": {
                    "summary": "Summary",
                    "issuetype": issue_type,
                    "status": {"name": "Open"}
                }
            }))
            .unwrap();

            assert!(render_issue(&issue, &[]).contains("- Type: Unknown\n"));
        }
    }

    #[test]
    fn created_timestamps_render_verbatim() {
        for created in [
            "2026-10-17T02:13:45.123+0200",
            "2026-10-17T00:13:45Z",
            "2026-10-17T02:13:45.123-0330",
        ] {
            let mut issue = issue(None);
            issue.fields.created = Some(created.to_string());

            assert!(
                render_issue(&issue, &[]).contains(&format!("\n- Created: {created}\n")),
                "{created}"
            );
        }
    }

    #[test]
    fn unusual_created_values_and_issue_types_are_escaped_and_single_line() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary",
                "issuetype": {"name": "[Bug](x)\n# forged"},
                "status": {"name": "Open"},
                "created": "2026-10-17 *today*\u{1b}[31m\n# forged",
                "description": null
            }
        }))
        .unwrap();
        let output = render_issue(&issue, &[]);

        assert!(output.contains("- Type: \\[Bug\\]\\(x\\) \\# forged\n"));
        assert!(output.contains("- Created: 2026-10-17 \\*today\\*\\[31m \\# forged\n"));
        assert!(!output.contains('\u{1b}'));
    }

    #[test]
    fn unicode_line_separators_in_metadata_become_spaces() {
        let issue: Issue = serde_json::from_value(json!({
            "key": "ABC-123",
            "fields": {
                "summary": "Summary\u{2028}# forged",
                "issuetype": {"name": "Bug\u{2029}details"},
                "status": {"name": "Open"},
                "description": null
            }
        }))
        .unwrap();
        let output = render_issue(&issue, &[]);

        assert!(output.contains("ABC-123: Summary \\# forged"));
        assert!(output.contains("- Type: Bug details\n"));
        assert!(!output.contains(['\u{2028}', '\u{2029}']));
    }

    fn issue_link(relation: &str, key: &str, summary: &str, status: &str) -> IssueLink {
        IssueLink {
            relation: Some(relation.to_string()),
            key: Some(key.to_string()),
            summary: Some(summary.to_string()),
            status: Some(status.to_string()),
        }
    }

    fn remote_link(title: Option<&str>, url: Option<&str>) -> RemoteLink {
        RemoteLink {
            title: title.map(str::to_string),
            url: url.map(str::to_string),
        }
    }

    fn links_section(issue_links: Vec<IssueLink>, remote_links: &[RemoteLink]) -> String {
        let mut issue = issue(None);
        issue.fields.issue_links = issue_links;
        let output = render_issue(&issue, remote_links);
        let start = output.find("## Links\n\n").unwrap();
        output[start..].to_string()
    }

    #[test]
    fn links_follow_the_description_as_the_last_section() {
        let mut issue = issue(None);
        issue.fields.issue_links = vec![issue_link("blocks", "ABC-2", "Other", "Done")];

        assert!(render_issue(&issue, &[]).ends_with(
            "- Attachments: 0\n\n## Description\n\nNo description.\n\n## Links\n\n- blocks ABC-2: Other (Done)\n"
        ));
    }

    #[test]
    fn renders_outward_and_inward_issue_links_then_remote_links_in_order() {
        let output = links_section(
            vec![
                issue_link("blocks", "ABC-2", "Other", "In Progress"),
                issue_link("is blocked by", "ABC-3", "Blocker", "Done"),
            ],
            &[
                remote_link(Some("PR 1"), Some("https://github.com/acme/repo/pull/1")),
                remote_link(Some("Docs"), Some("http://example.com/docs")),
            ],
        );

        assert_eq!(
            output,
            "## Links\n\n- blocks ABC-2: Other (In Progress)\n- is blocked by ABC-3: Blocker (Done)\n- Remote: PR 1 <https://github.com/acme/repo/pull/1>\n- Remote: Docs <http://example.com/docs>\n"
        );
    }

    #[test]
    fn renders_remote_links_without_issue_links() {
        let output = links_section(
            Vec::new(),
            &[remote_link(Some("PR 1"), Some("https://example.com/1"))],
        );

        assert_eq!(
            output,
            "## Links\n\n- Remote: PR 1 <https://example.com/1>\n"
        );
    }

    #[test]
    fn no_links_render_none() {
        assert_eq!(links_section(Vec::new(), &[]), "## Links\n\nNone\n");
    }

    #[test]
    fn missing_and_empty_link_values_render_unknown() {
        let missing = IssueLink {
            relation: None,
            key: None,
            summary: None,
            status: None,
        };
        let empty = issue_link("\u{1b}", "", "\r\n", " ");

        let output = links_section(
            vec![missing, empty],
            &[remote_link(None, None), remote_link(Some("\u{7}"), None)],
        );

        assert_eq!(
            output,
            "## Links\n\n- Unknown Unknown: Unknown (Unknown)\n- Unknown Unknown: Unknown (Unknown)\n- Remote: Unknown\n- Remote: Unknown\n"
        );
    }

    #[test]
    fn remote_links_show_only_the_title_for_unsafe_urls() {
        for url in [
            "javascript:alert(1)",
            "/relative/path",
            "ftp://example.com/file",
            "https:example.com",
            "https://example.com/\u{1b}[31m",
            "",
        ] {
            let output = links_section(Vec::new(), &[remote_link(Some("Title"), Some(url))]);

            assert_eq!(output, "## Links\n\n- Remote: Title\n", "{url:?}");
        }
    }

    #[test]
    fn remote_link_urls_escape_autolink_delimiters() {
        let output = links_section(
            Vec::new(),
            &[remote_link(
                Some("Title"),
                Some("https://example.com/a b<c>d"),
            )],
        );

        assert_eq!(
            output,
            "## Links\n\n- Remote: Title <https://example.com/a%20b%3Cc%3Ed>\n"
        );
    }

    #[test]
    fn link_values_are_single_line_and_escaped() {
        let output = links_section(
            vec![issue_link(
                "[blocks](x)\n# forged",
                "ABC-2\u{202e}",
                "*sum*\u{2028}mary",
                "> Done",
            )],
            &[remote_link(
                Some("<b>PR</b>\r\n- item"),
                Some("https://example.com/"),
            )],
        );

        assert_eq!(
            output,
            "## Links\n\n- \\[blocks\\]\\(x\\) \\# forged ABC-2: \\*sum\\* mary (\\> Done)\n- Remote: \\<b\\>PR\\</b\\>  - item <https://example.com/>\n"
        );
        assert!(!output.contains(['\u{1b}', '\u{202e}', '\u{2028}']));
    }
}

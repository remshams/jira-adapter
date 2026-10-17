use std::cmp::Ordering;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Envelope of Xray's GraphQL endpoint. `errors` may be present even though
/// the HTTP status is 200, so it is checked before the data is used. The
/// message text is parsed but never rendered; errors are reported
/// generically.
#[derive(Debug, Deserialize)]
pub struct GetTestRunsResponse {
    pub data: Option<GetTestRunsData>,
    #[serde(default)]
    pub errors: Vec<GraphQlError>,
}

/// A GraphQL error entry. `message` is kept so the shape stays explicit,
/// but it is deliberately never included in error output.
#[derive(Debug, Clone, Deserialize)]
pub struct GraphQlError {
    #[allow(dead_code)]
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct GetTestRunsData {
    #[serde(rename = "getTestRuns")]
    pub get_test_runs: Option<TestRunPage>,
}

/// The fixed first page of `getTestRuns`: limit 100, start 0. Xray makes no
/// ordering guarantee for the returned runs.
///
/// Nullability follows the official GraphQL schema: `total`, `start`,
/// `limit`, and `results` are nullable, and so is every entry inside
/// `results`. A null list or a null entry is preserved as-is instead of
/// being silently coerced into an empty value.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunPage {
    pub total: Option<i64>,
    pub start: Option<i64>,
    pub limit: Option<i64>,
    pub results: Option<Vec<Option<TestRun>>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRun {
    pub id: Option<String>,
    pub status: Option<TestRunStatus>,
    pub started_on: Option<String>,
    pub finished_on: Option<String>,
    pub test_execution: Option<TestExecutionRef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TestRunStatus {
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestExecutionRef {
    pub issue_id: Option<String>,
}

/// Field that can order the returned page. Clap renders variant names as
/// kebab-case values, so this is `--sort started-on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TestRunSortField {
    /// The instant the run was started
    StartedOn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TestRunSortDirection {
    /// Oldest first
    Asc,
    /// Newest first
    Desc,
}

/// Sorts the returned page in place by the requested field. Xray makes no
/// ordering promise for the page, so this only reorders what the server
/// already returned; it can never surface runs outside the fixed first
/// page. Valid `startedOn` timestamps sort first, ordered by instant
/// (oldest first for asc, newest first for desc), then runs with a missing
/// or unparseable timestamp, then null entries. Equal instants break ties
/// by run ID (null IDs last) and finally by original position, so the
/// result is deterministic in both directions.
pub(crate) fn sort_page(
    page: &mut TestRunPage,
    field: TestRunSortField,
    direction: TestRunSortDirection,
) {
    let ascending = matches!(direction, TestRunSortDirection::Asc);
    let Some(results) = page.results.as_mut() else {
        return;
    };
    let mut entries: Vec<(usize, SortKey, Option<TestRun>)> = results
        .drain(..)
        .enumerate()
        .map(|(position, run)| {
            let key = sort_key(&run, field);
            (position, key, run)
        })
        .collect();
    entries.sort_by(|(position_a, key_a, run_a), (position_b, key_b, run_b)| {
        compare_keys(key_a, key_b, ascending)
            .then_with(|| compare_ids(run_a, run_b))
            .then_with(|| position_a.cmp(position_b))
    });
    results.extend(entries.into_iter().map(|(_, _, run)| run));
}

/// Sort classes. `Unparseable` covers both a missing and an invalid
/// timestamp; both lose to any parseable instant regardless of direction.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SortKey {
    Instant(OffsetDateTime),
    Unparseable,
    Null,
}

fn sort_key(entry: &Option<TestRun>, field: TestRunSortField) -> SortKey {
    let Some(run) = entry else {
        return SortKey::Null;
    };
    match field {
        TestRunSortField::StartedOn => match run.started_on.as_deref() {
            Some(raw) => {
                OffsetDateTime::parse(raw, &Rfc3339).map_or(SortKey::Unparseable, SortKey::Instant)
            }
            None => SortKey::Unparseable,
        },
    }
}

fn compare_keys(a: &SortKey, b: &SortKey, ascending: bool) -> Ordering {
    match (a, b) {
        (SortKey::Instant(a), SortKey::Instant(b)) => compare_instants(a, b, ascending),
        _ => class_rank(a).cmp(&class_rank(b)),
    }
}

/// Orders sort classes independent of the direction: instants first, then
/// unparseable timestamps, then null entries.
fn class_rank(key: &SortKey) -> u8 {
    match key {
        SortKey::Instant(_) => 0,
        SortKey::Unparseable => 1,
        SortKey::Null => 2,
    }
}

fn compare_instants(a: &OffsetDateTime, b: &OffsetDateTime, ascending: bool) -> Ordering {
    // OffsetDateTime orders by instant, so runs compare correctly across
    // UTC offsets.
    let ordering = a.cmp(b);
    if ascending {
        ordering
    } else {
        ordering.reverse()
    }
}

fn compare_ids(a: &Option<TestRun>, b: &Option<TestRun>) -> Ordering {
    let a_id = a.as_ref().and_then(|run| run.id.as_deref());
    let b_id = b.as_ref().and_then(|run| run.id.as_deref());
    // Null IDs sort after present IDs, independent of the direction.
    match (a_id, b_id) {
        (Some(a), Some(b)) => a.cmp(b),
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
    }
}

/// Stable JSON output of `jcli xray test-runs`. Nullable values stay null
/// instead of turning into empty strings; `truncated` is null when the
/// server did not report a total to compare against.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunsOutput {
    pub test_key: String,
    pub total: Option<i64>,
    pub returned: usize,
    pub truncated: Option<bool>,
    pub start: Option<i64>,
    pub limit: Option<i64>,
    pub runs: Vec<Option<TestRunOutput>>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunOutput {
    pub id: Option<String>,
    pub status: Option<String>,
    pub started_on: Option<String>,
    pub finished_on: Option<String>,
    pub test_execution_issue_id: Option<String>,
}

/// Bidi formatting code points that can spoof terminal output. They are
/// not control characters, so serde_json leaves them literal.
const BIDI_CODE_POINTS: &[char] = &[
    '\u{061C}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202B}', '\u{202C}', '\u{202D}', '\u{202E}',
    '\u{2066}', '\u{2067}', '\u{2068}', '\u{2069}',
];

impl TestRunsOutput {
    /// Builds the CLI output from one page. `returned` counts every entry
    /// of a non-null results list, including null entries; a null results
    /// list counts as zero. `truncated` compares the reported total with
    /// that count and is null when no total is available.
    pub fn from_page(test_key: &str, page: &TestRunPage) -> Self {
        let runs = page
            .results
            .iter()
            .flatten()
            .map(|entry| {
                entry.as_ref().map(|run| TestRunOutput {
                    id: run.id.clone(),
                    status: run.status.as_ref().and_then(|status| status.name.clone()),
                    started_on: run.started_on.clone(),
                    finished_on: run.finished_on.clone(),
                    test_execution_issue_id: run
                        .test_execution
                        .as_ref()
                        .and_then(|execution| execution.issue_id.clone()),
                })
            })
            .collect();
        let returned = page.results.as_ref().map_or(0, |results| results.len());
        TestRunsOutput {
            test_key: test_key.to_owned(),
            total: page.total,
            returned,
            truncated: page.total.map(|total| total > returned as i64),
            start: page.start,
            limit: page.limit,
            runs,
        }
    }

    /// Renders the output as stable pretty JSON with one trailing newline.
    /// serde_json escapes C0 controls below U+0020 but leaves DEL and the
    /// C1 range (for example U+009B CSI and U+009D OSC) literal; those never
    /// occur as JSON structure, so escaping them cannot break the document.
    /// Bidi formatting code points are escaped the same way. Parsing the
    /// output recovers the original value.
    pub fn to_pretty_json(&self) -> String {
        let json = serde_json::to_string_pretty(self).expect("output should serialize");
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(id: &str) -> TestRun {
        TestRun {
            id: Some(id.to_owned()),
            status: Some(TestRunStatus {
                name: Some("PASSED".to_owned()),
            }),
            started_on: None,
            finished_on: None,
            test_execution: None,
        }
    }

    fn page_with(total: Option<i64>, results: Option<Vec<Option<TestRun>>>) -> TestRunPage {
        TestRunPage {
            total,
            start: Some(0),
            limit: Some(100),
            results,
        }
    }

    #[test]
    fn output_preserves_nullables_and_page_numbers() {
        let page = TestRunPage {
            total: Some(2),
            start: Some(0),
            limit: Some(100),
            results: Some(vec![Some(TestRun {
                id: Some("run-1".to_owned()),
                status: Some(TestRunStatus { name: None }),
                started_on: None,
                finished_on: Some("2024-05-01T10:00:00+02:00".to_owned()),
                test_execution: Some(TestExecutionRef {
                    issue_id: Some("10500".to_owned()),
                }),
            })]),
        };

        let output = TestRunsOutput::from_page("TEST-123", &page);

        assert_eq!(output.test_key, "TEST-123");
        assert_eq!(output.total, Some(2));
        assert_eq!(output.returned, 1);
        assert_eq!(output.truncated, Some(true));
        assert_eq!(output.start, Some(0));
        assert_eq!(output.limit, Some(100));
        let run = output.runs[0].as_ref().unwrap();
        assert_eq!(run.id.as_deref(), Some("run-1"));
        assert_eq!(run.status, None);
        assert_eq!(run.started_on, None);
        assert_eq!(
            run.finished_on.as_deref(),
            Some("2024-05-01T10:00:00+02:00")
        );
        assert_eq!(run.test_execution_issue_id.as_deref(), Some("10500"));
    }

    #[test]
    fn truncation_compares_total_with_the_number_returned() {
        let full_page = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(
                Some(100),
                Some(
                    (0..100)
                        .map(|index| Some(run_with(&index.to_string())))
                        .collect(),
                ),
            ),
        );
        assert_eq!(full_page.truncated, Some(false));

        let shorter_page = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(
                Some(5),
                Some(vec![Some(run_with("1")), None, Some(run_with("3"))]),
            ),
        );
        assert_eq!(shorter_page.truncated, Some(true));
        // Null entries count towards `returned`.
        assert_eq!(shorter_page.returned, 3);
        assert_eq!(shorter_page.runs.len(), 3);
        assert!(shorter_page.runs[1].is_none());

        let empty_page =
            TestRunsOutput::from_page("TEST-123", &page_with(Some(0), Some(Vec::new())));
        assert_eq!(empty_page.truncated, Some(false));
        assert_eq!(empty_page.returned, 0);
    }

    #[test]
    fn a_missing_total_makes_truncation_null() {
        let page = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(None, Some(vec![Some(run_with("1"))])),
        );

        assert_eq!(page.total, None);
        assert_eq!(page.truncated, None);
        assert_eq!(page.returned, 1);
    }

    #[test]
    fn a_null_results_list_and_null_entries_are_preserved() {
        let output = TestRunsOutput::from_page("TEST-123", &page_with(Some(2), None));
        assert_eq!(output.returned, 0);
        assert_eq!(output.runs, Vec::new());
        assert_eq!(output.truncated, Some(true));

        let parsed: serde_json::Value = serde_json::from_str(
            &TestRunsOutput::from_page(
                "TEST-123",
                &page_with(Some(2), Some(vec![None, Some(run_with("2"))])),
            )
            .to_pretty_json(),
        )
        .unwrap();
        assert!(parsed["runs"][0].is_null());
        assert_eq!(parsed["runs"][1]["id"], "2");
        assert_eq!(parsed["returned"], 2);
    }

    #[test]
    fn a_null_run_id_stays_null() {
        let output = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(
                Some(1),
                Some(vec![Some(TestRun {
                    id: None,
                    status: None,
                    started_on: None,
                    finished_on: None,
                    test_execution: Some(TestExecutionRef { issue_id: None }),
                })]),
            ),
        );

        let run = output.runs[0].as_ref().unwrap();
        assert_eq!(run.id, None);
        assert_eq!(run.test_execution_issue_id, None);

        let parsed: serde_json::Value = serde_json::from_str(&output.to_pretty_json()).unwrap();
        assert!(parsed["runs"][0]["id"].is_null());
        assert!(parsed["runs"][0]["testExecutionIssueId"].is_null());
    }

    #[test]
    fn json_output_is_pretty_and_camel_cased() {
        let output = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(Some(1), Some(vec![Some(run_with("0"))])),
        );
        let json = output.to_pretty_json();

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["testKey"], "TEST-123");
        assert_eq!(value["total"], 1);
        assert_eq!(value["returned"], 1);
        assert_eq!(value["truncated"], false);
        assert_eq!(value["start"], 0);
        assert_eq!(value["limit"], 100);
        assert_eq!(value["runs"][0]["id"], "0");
        assert!(json.ends_with("}\n"));
    }

    #[test]
    fn pretty_json_escapes_bidi_code_points_but_parses_to_the_same_value() {
        let status_with_bidi = "PASSED\u{202E}dessaP\u{200F}".to_owned();
        let output = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(
                Some(1),
                Some(vec![Some(TestRun {
                    id: Some("0".to_owned()),
                    status: Some(TestRunStatus {
                        name: Some(status_with_bidi.clone()),
                    }),
                    started_on: None,
                    finished_on: None,
                    test_execution: None,
                })]),
            ),
        );

        let json = output.to_pretty_json();

        // No raw bidi formatting code point reaches the rendered bytes;
        // each appears only as a literal `\uXXXX` escape sequence.
        for bidi in BIDI_CODE_POINTS {
            assert!(!json.contains(*bidi), "raw bidi char {bidi:?} leaked");
        }
        assert!(json.contains("\\u202E"));
        assert!(json.contains("\\u200F"));

        // Parsing recovers the original value unchanged.
        let parsed: TestRunsOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.runs[0].as_ref().unwrap().status.as_deref(),
            Some(status_with_bidi.as_str())
        );
    }

    #[test]
    fn pretty_json_escapes_c1_and_del_control_characters() {
        // serde_json only escapes C0 controls below U+0020; C1 codes such as
        // U+009B (CSI) and U+009D (OSC) plus DEL would otherwise reach the
        // terminal as raw bytes.
        let status_with_c1 = "PASSED\u{009B}4m\u{009D}\u{009C}\u{007F}".to_owned();
        let output = TestRunsOutput::from_page(
            "TEST-123",
            &page_with(
                Some(1),
                Some(vec![Some(TestRun {
                    id: Some("0".to_owned()),
                    status: Some(TestRunStatus {
                        name: Some(status_with_c1.clone()),
                    }),
                    started_on: None,
                    finished_on: None,
                    test_execution: None,
                })]),
            ),
        );

        let json = output.to_pretty_json();

        for control in ['\u{009B}', '\u{009D}', '\u{009C}', '\u{007F}'] {
            assert!(!json.contains(control), "raw control {control:?} leaked");
        }
        assert!(json.contains("\\u009B"));
        assert!(json.contains("\\u009D"));
        assert!(json.contains("\\u009C"));
        assert!(json.contains("\\u007F"));

        // Parsing recovers the original value unchanged.
        let parsed: TestRunsOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.runs[0].as_ref().unwrap().status.as_deref(),
            Some(status_with_c1.as_str())
        );
    }

    fn run_at(id: &str, started_on: Option<&str>) -> Option<TestRun> {
        Some(TestRun {
            id: Some(id.to_owned()),
            status: None,
            started_on: started_on.map(str::to_owned),
            finished_on: None,
            test_execution: None,
        })
    }

    fn sorted_ids(results: &[Option<TestRun>]) -> Vec<Option<&str>> {
        results
            .iter()
            .map(|run| run.as_ref().and_then(|run| run.id.as_deref()))
            .collect()
    }

    // run-a and run-b name the same instant through different offsets;
    // run-c is older, run-d has a garbage timestamp, run-e has none.
    fn offset_page_input() -> Vec<Option<TestRun>> {
        vec![
            run_at("run-b", Some("2024-05-02T09:00:00+01:00")),
            run_at("run-d", Some("not-a-timestamp")),
            run_at("run-a", Some("2024-05-02T10:00:00+02:00")),
            run_at("run-e", None),
            run_at("run-c", Some("2024-05-01T12:00:00Z")),
        ]
    }

    #[test]
    fn sorting_compares_instants_across_utc_offsets() {
        let mut page = page_with(Some(5), Some(offset_page_input()));

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Asc,
        );

        assert_eq!(
            sorted_ids(page.results.as_ref().unwrap()),
            [
                Some("run-c"),
                Some("run-a"),
                Some("run-b"),
                Some("run-d"),
                Some("run-e")
            ]
        );
    }

    #[test]
    fn desc_reverses_instants_but_keeps_invalid_and_null_last() {
        let mut page = page_with(Some(5), Some(offset_page_input()));

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Desc,
        );

        assert_eq!(
            sorted_ids(page.results.as_ref().unwrap()),
            [
                Some("run-a"),
                Some("run-b"),
                Some("run-c"),
                Some("run-d"),
                Some("run-e")
            ]
        );
    }

    #[test]
    fn null_entries_sort_after_every_non_null_entry() {
        let mut page = page_with(
            Some(3),
            Some(vec![
                None,
                run_at("run-1", Some("2024-05-01T12:00:00Z")),
                run_at("run-2", Some("not-a-timestamp")),
            ]),
        );

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Asc,
        );

        assert_eq!(
            sorted_ids(page.results.as_ref().unwrap()),
            [Some("run-1"), Some("run-2"), None]
        );
    }

    #[test]
    fn equal_instants_break_ties_by_id_then_original_position() {
        // All three runs name the same instant; run-d has a null ID.
        let mut page = page_with(
            Some(4),
            Some(vec![
                run_at("run-b", Some("2024-05-01T12:00:00Z")),
                run_at("run-a", Some("2024-05-01T14:00:00+02:00")),
                run_at("run-a", Some("2024-05-01T10:00:00-02:00")),
                Some(TestRun {
                    id: None,
                    status: None,
                    started_on: Some("2024-05-01T12:00:00Z".to_owned()),
                    finished_on: None,
                    test_execution: None,
                }),
            ]),
        );

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Asc,
        );

        // Equal IDs keep their original relative order; the null ID lands
        // last without changing the direction.
        assert_eq!(
            sorted_ids(page.results.as_ref().unwrap()),
            [Some("run-a"), Some("run-a"), Some("run-b"), None]
        );
    }

    #[test]
    fn sorting_leaves_page_numbers_and_metadata_untouched() {
        let mut page = page_with(
            Some(7),
            Some(vec![run_at("run-1", Some("2024-05-01T12:00:00Z"))]),
        );
        page.start = Some(20);
        page.limit = Some(100);

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Desc,
        );

        assert_eq!(page.total, Some(7));
        assert_eq!(page.start, Some(20));
        assert_eq!(page.limit, Some(100));
    }

    #[test]
    fn a_null_results_list_is_left_alone_by_sorting() {
        let mut page = page_with(Some(0), None);

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Asc,
        );

        assert!(page.results.is_none());
    }
}

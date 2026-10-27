use std::cmp::Ordering;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::output;
use crate::output::terminal_field;

/// The kind of Xray issue that `getTestRuns` can filter by. It follows from
/// the Jira issue type name and decides which filter the query uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum XrayIssueKind {
    /// A Test issue: its runs across all Test Executions.
    Test,
    /// A Test Execution or Sub Test Execution issue: the runs it contains.
    TestExecution,
}

/// The issue exists but its type is none of Xray's default `Test`,
/// `Test Execution` and `Sub Test Execution` types.
#[derive(Debug, PartialEq, Eq, Error)]
#[error("{issue_key} is not an Xray Test or Test Execution (issue type: {issue_type})")]
pub struct NotAnXrayIssue {
    issue_key: String,
    issue_type: String,
}

impl XrayIssueKind {
    /// Maps a Jira issue type name to the Xray kind. Matches Xray's default
    /// type names exactly and case-sensitively; any other or missing name is
    /// an error that names the issue and its type on one terminal-safe line.
    pub fn for_issue(issue_key: &str, issue_type: Option<&str>) -> Result<Self, NotAnXrayIssue> {
        match issue_type {
            Some("Test") => Ok(Self::Test),
            Some("Test Execution" | "Sub Test Execution") => Ok(Self::TestExecution),
            _ => Err(NotAnXrayIssue {
                issue_key: issue_key.to_owned(),
                issue_type: describe_issue_type(issue_type),
            }),
        }
    }
}

/// The issue type name for the error message. The name is Jira data, so it
/// is reduced to one safe line; a missing or blank name reads `unknown`.
fn describe_issue_type(issue_type: Option<&str>) -> String {
    terminal_field(issue_type, "unknown").trim().to_owned()
}

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

/// A page of `getTestRuns`. The client merges the pages it fetched into one
/// such page: the runs of all pages in fetch order, the last page's total,
/// start 0 and the requested limit. Xray makes no ordering guarantee for
/// the returned runs.
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
#[cfg_attr(test, derive(Default))]
#[serde(rename_all = "camelCase")]
pub struct TestRun {
    pub id: Option<String>,
    pub status: Option<TestRunStatus>,
    pub started_on: Option<String>,
    pub finished_on: Option<String>,
    pub test_execution: Option<TestExecutionRef>,
    // The detail fields below are only selected with `--details`; the
    // default query leaves them out, so they deserialize as `None`.
    pub comment: Option<String>,
    /// Issue IDs of the defects linked to the run.
    pub defects: Option<Vec<Option<String>>>,
    pub evidence: Option<Vec<Option<Evidence>>>,
    pub steps: Option<Vec<Option<TestRunStep>>>,
    /// Results of automated tests, such as JUnit test cases.
    pub results: Option<Vec<Option<TestRunResult>>>,
}

/// A status with its name. Xray's `Status` of a run and `StepStatus` of a
/// step or result both select only `name`, so they share this model.
#[derive(Debug, Clone, Deserialize)]
pub struct TestRunStatus {
    pub name: Option<String>,
}

/// A file attached to a run or step. jcli passes `downloadLink` through and
/// never downloads the file.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub filename: Option<String>,
    pub download_link: Option<String>,
}

/// Xray's `TestRunStep`: the outcome of one manual test step.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunStep {
    pub status: Option<TestRunStatus>,
    pub actual_result: Option<String>,
    pub comment: Option<String>,
    pub evidence: Option<Vec<Option<Evidence>>>,
}

/// Xray's `Result`: one automated result imported into the run. `log` holds
/// the error or failure output; `duration` is passed through as a number.
#[derive(Debug, Clone, Deserialize)]
pub struct TestRunResult {
    pub name: Option<String>,
    pub status: Option<TestRunStatus>,
    pub log: Option<String>,
    pub duration: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestExecutionRef {
    pub issue_id: Option<String>,
}

/// Field that can order the returned runs. Clap renders variant names as
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

/// Sorts the returned runs in place by the requested field. Xray makes no
/// ordering promise for its pages, so this only reorders what the server
/// already returned, across all fetched pages; it can never surface runs
/// beyond the requested limit. Valid `startedOn` timestamps sort first,
/// ordered by instant (oldest first for asc, newest first for desc), then
/// runs with a missing or unparseable timestamp, then null entries. Equal
/// instants break ties by run ID (null IDs last) and finally by original
/// position, so the result is deterministic in both directions.
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
/// server did not report a total to compare against. `issue_kind` names the
/// filter that was queried, so an empty `runs` list means no runs for that
/// issue.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunsOutput {
    pub issue_key: String,
    pub issue_kind: XrayIssueKind,
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
    /// Present only with `--details`. Flattened, so its fields sit next to
    /// the ones above; without `--details` none of them appear.
    #[serde(flatten)]
    pub details: Option<TestRunDetailsOutput>,
}

/// The `--details` fields of a run. A field Xray leaves out is null, and a
/// null list entry stays null.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunDetailsOutput {
    pub comment: Option<String>,
    pub defects: Option<Vec<Option<String>>>,
    pub evidence: Option<Vec<Option<EvidenceOutput>>>,
    pub steps: Option<Vec<Option<TestRunStepOutput>>>,
    pub results: Option<Vec<Option<TestRunResultOutput>>>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceOutput {
    pub filename: Option<String>,
    pub download_link: Option<String>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestRunStepOutput {
    pub status: Option<String>,
    pub actual_result: Option<String>,
    pub comment: Option<String>,
    pub evidence: Option<Vec<Option<EvidenceOutput>>>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct TestRunResultOutput {
    pub name: Option<String>,
    pub status: Option<String>,
    pub log: Option<String>,
    pub duration: Option<f64>,
}

/// Maps a nullable list with nullable entries, keeping both kinds of null.
fn map_list<T, U>(list: &Option<Vec<Option<T>>>, map: impl Fn(&T) -> U) -> Option<Vec<Option<U>>> {
    list.as_ref().map(|entries| {
        entries
            .iter()
            .map(|entry| entry.as_ref().map(&map))
            .collect()
    })
}

fn status_name(status: &Option<TestRunStatus>) -> Option<String> {
    status.as_ref().and_then(|status| status.name.clone())
}

fn evidence_output(
    evidence: &Option<Vec<Option<Evidence>>>,
) -> Option<Vec<Option<EvidenceOutput>>> {
    map_list(evidence, |evidence| EvidenceOutput {
        filename: evidence.filename.clone(),
        download_link: evidence.download_link.clone(),
    })
}

impl TestRunDetailsOutput {
    fn from_run(run: &TestRun) -> Self {
        TestRunDetailsOutput {
            comment: run.comment.clone(),
            defects: run.defects.clone(),
            evidence: evidence_output(&run.evidence),
            steps: map_list(&run.steps, |step| TestRunStepOutput {
                status: status_name(&step.status),
                actual_result: step.actual_result.clone(),
                comment: step.comment.clone(),
                evidence: evidence_output(&step.evidence),
            }),
            results: map_list(&run.results, |result| TestRunResultOutput {
                name: result.name.clone(),
                status: status_name(&result.status),
                log: result.log.clone(),
                duration: result.duration,
            }),
        }
    }
}

impl TestRunsOutput {
    /// Builds the CLI output from the merged page. `returned` counts every
    /// entry of a non-null results list, including null entries; a null
    /// results list counts as zero. `truncated` compares the reported total with
    /// that count and is null when no total is available. `details` adds
    /// the `--details` fields to every run.
    pub fn from_page(
        issue_key: &str,
        issue_kind: XrayIssueKind,
        page: &TestRunPage,
        details: bool,
    ) -> Self {
        let runs = map_list(&page.results, |run| TestRunOutput {
            id: run.id.clone(),
            status: status_name(&run.status),
            started_on: run.started_on.clone(),
            finished_on: run.finished_on.clone(),
            test_execution_issue_id: run
                .test_execution
                .as_ref()
                .and_then(|execution| execution.issue_id.clone()),
            details: details.then(|| TestRunDetailsOutput::from_run(run)),
        })
        .unwrap_or_default();
        let returned = page.results.as_ref().map_or(0, |results| results.len());
        TestRunsOutput {
            issue_key: issue_key.to_owned(),
            issue_kind,
            total: page.total,
            returned,
            truncated: page.total.map(|total| total > returned as i64),
            start: page.start,
            limit: page.limit,
            runs,
        }
    }

    /// Renders the output as stable pretty JSON with one trailing newline,
    /// escaping C1 controls, DEL, and bidi code points.
    pub fn to_pretty_json(&self) -> String {
        output::to_pretty_json(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::BIDI_CODE_POINTS;

    /// Output without `--details`.
    fn summary(issue_key: &str, issue_kind: XrayIssueKind, page: &TestRunPage) -> TestRunsOutput {
        TestRunsOutput::from_page(issue_key, issue_kind, page, false)
    }

    fn run_with(id: &str) -> TestRun {
        TestRun {
            id: Some(id.to_owned()),
            status: Some(TestRunStatus {
                name: Some("PASSED".to_owned()),
            }),
            started_on: None,
            finished_on: None,
            test_execution: None,
            ..TestRun::default()
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
                ..TestRun::default()
            })]),
        };

        let output = summary("TEST-123", XrayIssueKind::Test, &page);

        assert_eq!(output.issue_key, "TEST-123");
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
        let full_page = summary(
            "TEST-123",
            XrayIssueKind::Test,
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

        let shorter_page = summary(
            "TEST-123",
            XrayIssueKind::Test,
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

        let empty_page = summary(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(Some(0), Some(Vec::new())),
        );
        assert_eq!(empty_page.truncated, Some(false));
        assert_eq!(empty_page.returned, 0);
    }

    #[test]
    fn a_merged_page_reports_the_requested_limit_and_truncation_over_all_runs() {
        let runs = (0..250).map(|index| Some(run_with(&index.to_string())));
        let mut page = page_with(Some(400), Some(runs.collect()));
        page.limit = Some(250);

        let output = summary("TEST-123", XrayIssueKind::Test, &page);

        assert_eq!(output.returned, 250);
        assert_eq!(output.runs.len(), 250);
        assert_eq!(output.truncated, Some(true));
        assert_eq!(output.start, Some(0));
        assert_eq!(output.limit, Some(250));

        page.total = Some(250);
        let all_runs = summary("TEST-123", XrayIssueKind::Test, &page);
        assert_eq!(all_runs.truncated, Some(false));
    }

    #[test]
    fn sorting_orders_runs_from_all_merged_pages() {
        // The newest run came on the last page, the oldest on the first.
        let mut page = page_with(
            Some(150),
            Some(
                (0..150)
                    .map(|index| {
                        let started_on =
                            format!("2024-05-01T{:02}:{:02}:00Z", index / 60, index % 60);
                        run_at(&format!("run-{index}"), Some(&started_on))
                    })
                    .collect(),
            ),
        );

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Desc,
        );

        let ids = sorted_ids(page.results.as_ref().unwrap());
        assert_eq!(ids.len(), 150);
        assert_eq!(ids[0], Some("run-149"));
        assert_eq!(ids[149], Some("run-0"));
    }

    #[test]
    fn a_missing_total_makes_truncation_null() {
        let page = summary(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(None, Some(vec![Some(run_with("1"))])),
        );

        assert_eq!(page.total, None);
        assert_eq!(page.truncated, None);
        assert_eq!(page.returned, 1);
    }

    #[test]
    fn a_null_results_list_and_null_entries_are_preserved() {
        let output = summary("TEST-123", XrayIssueKind::Test, &page_with(Some(2), None));
        assert_eq!(output.returned, 0);
        assert_eq!(output.runs, Vec::new());
        assert_eq!(output.truncated, Some(true));

        let parsed: serde_json::Value = serde_json::from_str(
            &summary(
                "TEST-123",
                XrayIssueKind::Test,
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
        let output = summary(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(
                Some(1),
                Some(vec![Some(TestRun {
                    id: None,
                    status: None,
                    started_on: None,
                    finished_on: None,
                    test_execution: Some(TestExecutionRef { issue_id: None }),
                    ..TestRun::default()
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
        let output = summary(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(Some(1), Some(vec![Some(run_with("0"))])),
        );
        let json = output.to_pretty_json();

        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["issueKey"], "TEST-123");
        assert_eq!(value["issueKind"], "test");
        assert_eq!(value["total"], 1);
        assert_eq!(value["returned"], 1);
        assert_eq!(value["truncated"], false);
        assert_eq!(value["start"], 0);
        assert_eq!(value["limit"], 100);
        assert_eq!(value["runs"][0]["id"], "0");
        assert!(json.ends_with("}\n"));
    }

    fn evidence(filename: &str) -> Option<Evidence> {
        Some(Evidence {
            filename: Some(filename.to_owned()),
            download_link: Some(format!("https://xray.example/{filename}")),
        })
    }

    /// A run with every detail field set, including null list entries.
    fn detailed_run() -> TestRun {
        TestRun {
            comment: Some("checkout failed".to_owned()),
            defects: Some(vec![Some("10600".to_owned()), None]),
            evidence: Some(vec![evidence("screen.png"), None]),
            steps: Some(vec![
                Some(TestRunStep {
                    status: Some(TestRunStatus {
                        name: Some("FAILED".to_owned()),
                    }),
                    actual_result: Some("HTTP 500".to_owned()),
                    comment: Some("retried once".to_owned()),
                    evidence: Some(vec![evidence("step.log")]),
                }),
                None,
            ]),
            results: Some(vec![
                Some(TestRunResult {
                    name: Some("checkout succeeds".to_owned()),
                    status: Some(TestRunStatus {
                        name: Some("FAILED".to_owned()),
                    }),
                    log: Some("AssertionError: expected 200, got 500".to_owned()),
                    duration: Some(1520.5),
                }),
                None,
            ]),
            ..run_with("run-1")
        }
    }

    fn rendered_runs(runs: Vec<Option<TestRun>>, details: bool) -> serde_json::Value {
        let output = TestRunsOutput::from_page(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(Some(1), Some(runs)),
            details,
        );
        let value: serde_json::Value = serde_json::from_str(&output.to_pretty_json()).unwrap();
        value["runs"].clone()
    }

    #[test]
    fn without_details_a_run_renders_only_the_summary_fields() {
        let output = TestRunsOutput::from_page(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(Some(1), Some(vec![Some(detailed_run())])),
            false,
        );

        assert_eq!(
            output.to_pretty_json(),
            r#"{
  "issueKey": "TEST-123",
  "issueKind": "test",
  "total": 1,
  "returned": 1,
  "truncated": false,
  "start": 0,
  "limit": 100,
  "runs": [
    {
      "id": "run-1",
      "status": "PASSED",
      "startedOn": null,
      "finishedOn": null,
      "testExecutionIssueId": null
    }
  ]
}
"#
        );
    }

    #[test]
    fn details_add_comment_defects_evidence_steps_and_results() {
        let runs = rendered_runs(vec![Some(detailed_run()), None], true);

        assert_eq!(
            runs,
            serde_json::json!([
                {
                    "id": "run-1",
                    "status": "PASSED",
                    "startedOn": null,
                    "finishedOn": null,
                    "testExecutionIssueId": null,
                    "comment": "checkout failed",
                    "defects": ["10600", null],
                    "evidence": [
                        {"filename": "screen.png", "downloadLink": "https://xray.example/screen.png"},
                        null
                    ],
                    "steps": [
                        {
                            "status": "FAILED",
                            "actualResult": "HTTP 500",
                            "comment": "retried once",
                            "evidence": [
                                {"filename": "step.log", "downloadLink": "https://xray.example/step.log"}
                            ]
                        },
                        null
                    ],
                    "results": [
                        {
                            "name": "checkout succeeds",
                            "status": "FAILED",
                            "log": "AssertionError: expected 200, got 500",
                            "duration": 1520.5
                        },
                        null
                    ]
                },
                null
            ])
        );
    }

    #[test]
    fn missing_detail_fields_are_null() {
        let run = TestRun {
            steps: Some(vec![Some(TestRunStep {
                status: Some(TestRunStatus { name: None }),
                actual_result: None,
                comment: None,
                evidence: None,
            })]),
            results: Some(vec![Some(TestRunResult {
                name: None,
                status: None,
                log: None,
                duration: None,
            })]),
            evidence: Some(vec![Some(Evidence {
                filename: None,
                download_link: None,
            })]),
            ..run_with("run-1")
        };

        let runs = rendered_runs(vec![Some(run)], true);

        assert!(runs[0]["comment"].is_null());
        assert!(runs[0]["defects"].is_null());
        assert_eq!(
            runs[0]["evidence"],
            serde_json::json!([{"filename": null, "downloadLink": null}])
        );
        assert_eq!(
            runs[0]["steps"],
            serde_json::json!([{
                "status": null,
                "actualResult": null,
                "comment": null,
                "evidence": null
            }])
        );
        assert_eq!(
            runs[0]["results"],
            serde_json::json!([{"name": null, "status": null, "log": null, "duration": null}])
        );
    }

    #[test]
    fn empty_detail_lists_stay_empty() {
        let run = TestRun {
            defects: Some(Vec::new()),
            evidence: Some(Vec::new()),
            steps: Some(Vec::new()),
            results: Some(Vec::new()),
            ..run_with("run-1")
        };

        let runs = rendered_runs(vec![Some(run)], true);

        for field in ["defects", "evidence", "steps", "results"] {
            assert_eq!(runs[0][field], serde_json::json!([]), "{field}");
        }
        assert!(runs[0]["comment"].is_null());
    }

    #[test]
    fn detail_strings_use_the_output_escaping() {
        let run = TestRun {
            results: Some(vec![Some(TestRunResult {
                name: None,
                status: None,
                log: Some("fail\u{202E}\u{009B}31m".to_owned()),
                duration: None,
            })]),
            ..run_with("run-1")
        };
        let output = TestRunsOutput::from_page(
            "TEST-123",
            XrayIssueKind::Test,
            &page_with(Some(1), Some(vec![Some(run)])),
            true,
        );

        let json = output.to_pretty_json();

        assert!(
            json.contains("\"log\": \"fail\\u202E\\u009B31m\""),
            "{json}"
        );
    }

    #[test]
    fn json_output_names_the_queried_issue_and_kind_first() {
        let output = summary(
            "EXEC-7",
            XrayIssueKind::TestExecution,
            &page_with(Some(0), Some(Vec::new())),
        );
        let json = output.to_pretty_json();

        assert!(
            json.starts_with(
                "{\n  \"issueKey\": \"EXEC-7\",\n  \"issueKind\": \"testExecution\",\n"
            ),
            "{json}"
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("testKey").is_none());
        assert_eq!(value["runs"], serde_json::json!([]));
    }

    #[test]
    fn pretty_json_escapes_bidi_code_points_but_parses_to_the_same_value() {
        let status_with_bidi = "PASSED\u{202E}dessaP\u{200F}".to_owned();
        let output = summary(
            "TEST-123",
            XrayIssueKind::Test,
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
                    ..TestRun::default()
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
        let output = summary(
            "TEST-123",
            XrayIssueKind::Test,
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
                    ..TestRun::default()
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
            ..TestRun::default()
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
    fn sort_classes_outrank_ids_and_original_position() {
        // IDs and input order both favor the reverse of the class order, so
        // only the class decides: instant, then unparseable, then null.
        let mut page = page_with(
            Some(4),
            Some(vec![
                run_at("run-1", None),
                None,
                Some(TestRun {
                    id: None,
                    status: None,
                    started_on: Some("not-a-timestamp".to_owned()),
                    finished_on: None,
                    test_execution: None,
                    ..TestRun::default()
                }),
                run_at("run-9", Some("2024-05-01T12:00:00Z")),
            ]),
        );

        sort_page(
            &mut page,
            TestRunSortField::StartedOn,
            TestRunSortDirection::Asc,
        );

        let results = page.results.as_ref().unwrap();
        assert_eq!(
            sorted_ids(results),
            [Some("run-9"), Some("run-1"), None, None]
        );
        // The unparseable run with a null ID precedes the null entry.
        assert!(results[2].is_some());
        assert!(results[3].is_none());
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
                    ..TestRun::default()
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

    fn error_message(issue_type: Option<&str>) -> String {
        XrayIssueKind::for_issue("ABC-1", issue_type)
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn default_xray_type_names_map_to_their_kind() {
        assert_eq!(
            XrayIssueKind::for_issue("ABC-1", Some("Test")),
            Ok(XrayIssueKind::Test)
        );
        assert_eq!(
            XrayIssueKind::for_issue("ABC-1", Some("Test Execution")),
            Ok(XrayIssueKind::TestExecution)
        );
        assert_eq!(
            XrayIssueKind::for_issue("ABC-1", Some("Sub Test Execution")),
            Ok(XrayIssueKind::TestExecution)
        );
    }

    #[test]
    fn type_names_match_exactly_and_case_sensitively() {
        for name in [
            "test",
            "TEST",
            "Test ",
            "test execution",
            "Test Plan",
            "Tests",
        ] {
            assert!(
                XrayIssueKind::for_issue("ABC-1", Some(name)).is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn other_types_name_the_issue_and_its_type() {
        assert_eq!(
            error_message(Some("Story")),
            "ABC-1 is not an Xray Test or Test Execution (issue type: Story)"
        );
        assert_eq!(
            error_message(Some("User Story")),
            "ABC-1 is not an Xray Test or Test Execution (issue type: User Story)"
        );
    }

    #[test]
    fn a_missing_or_blank_type_falls_back_to_unknown() {
        for issue_type in [None, Some(""), Some("  "), Some("\u{1b}\u{202E}")] {
            assert_eq!(
                error_message(issue_type),
                "ABC-1 is not an Xray Test or Test Execution (issue type: unknown)",
                "{issue_type:?}"
            );
        }
    }

    #[test]
    fn the_type_name_is_reduced_to_one_terminal_safe_line() {
        assert_eq!(
            error_message(Some(" Bug\r\nreport\u{1b}[31m\u{202E} ")),
            "ABC-1 is not an Xray Test or Test Execution (issue type: Bug report[31m)"
        );
    }

    #[test]
    fn kinds_serialize_as_camel_case_names() {
        assert_eq!(
            serde_json::to_value(XrayIssueKind::Test).unwrap(),
            serde_json::json!("test")
        );
        assert_eq!(
            serde_json::to_value(XrayIssueKind::TestExecution).unwrap(),
            serde_json::json!("testExecution")
        );
    }
}

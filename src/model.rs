//! Domain types for the inbox, and the conversion from GitHub's GraphQL shape.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::config::GroupConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Awaiting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failing,
    Pending,
    Passing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSummary {
    pub passed: u64,
    pub total: u64,
    pub state: CheckState,
}

#[derive(Debug, Clone)]
pub struct PrSummary {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub updated_at: DateTime<Utc>,
    pub is_draft: bool,
    pub review: ReviewState,
    pub checks: Option<CheckSummary>,
    pub comments: u64,
}

#[derive(Debug, Clone)]
pub struct Group {
    pub title: String,
    /// GitHub's match count. It can exceed `prs.len()` when a search hits the page limit.
    pub total: u64,
    pub prs: Vec<PrSummary>,
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckLine {
    pub name: String,
    pub state: CheckState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewer {
    pub name: String,
    /// GitHub's review state in lower case, or "requested" for a pending request.
    pub state: String,
}

#[derive(Debug, Clone)]
pub struct Activity {
    pub author: String,
    pub at: DateTime<Utc>,
    pub action: String,
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct PrDetail {
    pub summary: PrSummary,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub base: String,
    pub head: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub reviewers: Vec<Reviewer>,
    /// Failing checks first, then pending, then passing.
    pub checks: Vec<CheckLine>,
    /// Newest first.
    pub activity: Vec<Activity>,
}

// ---- GitHub GraphQL shapes ----

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct Name {
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepoRef {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: String,
    contexts: RollupContexts,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollupContexts {
    total_count: u64,
    check_run_counts_by_state: Vec<StateCount>,
    status_context_counts_by_state: Vec<StateCount>,
    #[serde(default)]
    nodes: Vec<RawCheck>,
}

#[derive(Deserialize)]
struct StateCount {
    state: String,
    count: u64,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum RawCheck {
    CheckRun {
        name: String,
        conclusion: Option<String>,
        status: String,
    },
    StatusContext {
        context: String,
        state: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRow {
    number: u64,
    title: String,
    url: String,
    is_draft: bool,
    updated_at: DateTime<Utc>,
    review_decision: Option<String>,
    total_comments_count: Option<u64>,
    author: Option<Login>,
    repository: RepoRef,
    commits: Nodes<CommitNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSearch {
    issue_count: u64,
    nodes: Vec<RawRow>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDetail {
    #[serde(flatten)]
    row: RawRow,
    body: String,
    created_at: DateTime<Utc>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    base_ref_name: String,
    head_ref_name: String,
    labels: Nodes<Name>,
    assignees: Nodes<Login>,
    review_requests: Nodes<RawRequest>,
    latest_opinionated_reviews: Nodes<RawReview>,
    comments: Nodes<RawComment>,
    reviews: Nodes<RawReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequest {
    requested_reviewer: Option<RawReviewer>,
}

/// A user, bot or mannequin carries `login`; a team carries `name`.
#[derive(Deserialize)]
struct RawReviewer {
    login: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    author: Option<Login>,
    state: String,
    created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    author: Option<Login>,
    created_at: DateTime<Utc>,
    body: String,
}

// ---- Conversion ----

/// `data` is the GraphQL `data` object, holding one `g<index>` search per group.
pub fn parse_inbox(data: &Value, groups: &[GroupConfig]) -> Result<Vec<Group>> {
    groups
        .iter()
        .enumerate()
        .map(|(index, config)| parse_group(data, index, config))
        .collect()
}

fn parse_group(data: &Value, index: usize, config: &GroupConfig) -> Result<Group> {
    let key = format!("g{index}");
    let raw = data
        .get(&key)
        .with_context(|| format!("response has no {key} field"))?;
    let search: RawSearch = serde_json::from_value(raw.clone())
        .with_context(|| format!("parse group \"{}\"", config.title))?;
    Ok(Group {
        title: config.title.clone(),
        total: search.issue_count,
        prs: search.nodes.into_iter().map(summary_from).collect(),
        collapsed: config.collapsed,
    })
}

/// `pull_request` is `data.repository.pullRequest`.
pub fn parse_detail(pull_request: &Value) -> Result<PrDetail> {
    let raw: RawDetail =
        serde_json::from_value(pull_request.clone()).context("parse pull request")?;
    let checks = check_lines(&raw.row);
    let reviewers = reviewers_from(&raw.review_requests, &raw.latest_opinionated_reviews);
    let activity = activity_from(&raw.comments, &raw.reviews);
    Ok(PrDetail {
        summary: summary_from(raw.row),
        body: raw.body,
        created_at: raw.created_at,
        base: raw.base_ref_name,
        head: raw.head_ref_name,
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        labels: raw
            .labels
            .nodes
            .into_iter()
            .map(|label| label.name)
            .collect(),
        assignees: raw
            .assignees
            .nodes
            .into_iter()
            .map(|user| user.login)
            .collect(),
        reviewers,
        checks,
        activity,
    })
}

fn summary_from(raw: RawRow) -> PrSummary {
    let checks = latest_rollup(&raw).and_then(check_summary);
    PrSummary {
        repo: raw.repository.name_with_owner,
        number: raw.number,
        title: raw.title,
        url: raw.url,
        author: login_or_ghost(raw.author),
        updated_at: raw.updated_at,
        is_draft: raw.is_draft,
        review: review_state(raw.review_decision.as_deref()),
        checks,
        comments: raw.total_comments_count.unwrap_or(0),
    }
}

fn login_or_ghost(author: Option<Login>) -> String {
    // GitHub returns a null author for deleted accounts and shows them as "ghost".
    author.map_or_else(|| "ghost".to_string(), |user| user.login)
}

/// GitHub's web inbox shows "Awaiting approval" for PRs whose API decision is null,
/// so a null decision maps to the same state.
fn review_state(decision: Option<&str>) -> ReviewState {
    match decision {
        Some("APPROVED") => ReviewState::Approved,
        Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
        _ => ReviewState::Awaiting,
    }
}

fn latest_rollup(raw: &RawRow) -> Option<&Rollup> {
    raw.commits
        .nodes
        .last()?
        .commit
        .status_check_rollup
        .as_ref()
}

fn check_summary(rollup: &Rollup) -> Option<CheckSummary> {
    let contexts = &rollup.contexts;
    if contexts.total_count == 0 {
        return None;
    }
    let passed_runs = count_states(
        &contexts.check_run_counts_by_state,
        &["SUCCESS", "NEUTRAL", "SKIPPED"],
    );
    let passed_statuses = count_states(&contexts.status_context_counts_by_state, &["SUCCESS"]);
    Some(CheckSummary {
        passed: passed_runs + passed_statuses,
        total: contexts.total_count,
        state: rollup_state(&rollup.state),
    })
}

fn count_states(counts: &[StateCount], states: &[&str]) -> u64 {
    counts
        .iter()
        .filter(|entry| states.contains(&entry.state.as_str()))
        .map(|entry| entry.count)
        .sum()
}

fn rollup_state(state: &str) -> CheckState {
    match state {
        "SUCCESS" => CheckState::Passing,
        "PENDING" | "EXPECTED" => CheckState::Pending,
        _ => CheckState::Failing,
    }
}

fn check_lines(raw: &RawRow) -> Vec<CheckLine> {
    let Some(rollup) = latest_rollup(raw) else {
        return Vec::new();
    };
    let mut lines: Vec<CheckLine> = rollup.contexts.nodes.iter().map(check_line).collect();
    lines.sort_by(|left, right| {
        left.state
            .cmp(&right.state)
            .then(left.name.cmp(&right.name))
    });
    lines
}

fn check_line(check: &RawCheck) -> CheckLine {
    match check {
        RawCheck::CheckRun {
            name,
            conclusion,
            status,
        } => CheckLine {
            name: name.clone(),
            state: check_run_state(status, conclusion.as_deref()),
        },
        RawCheck::StatusContext { context, state } => CheckLine {
            name: context.clone(),
            state: rollup_state(state),
        },
    }
}

fn check_run_state(status: &str, conclusion: Option<&str>) -> CheckState {
    if status != "COMPLETED" {
        return CheckState::Pending;
    }
    match conclusion {
        Some("SUCCESS" | "NEUTRAL" | "SKIPPED") => CheckState::Passing,
        _ => CheckState::Failing,
    }
}

/// Pending requests come first. A review by someone who is requested again is dropped,
/// because the new request is the state that needs action.
fn reviewers_from(requests: &Nodes<RawRequest>, reviews: &Nodes<RawReview>) -> Vec<Reviewer> {
    let mut reviewers: Vec<Reviewer> = requests
        .nodes
        .iter()
        .filter_map(|request| request.requested_reviewer.as_ref())
        .filter_map(|reviewer| reviewer.login.clone().or_else(|| reviewer.name.clone()))
        .map(|name| Reviewer {
            name,
            state: "requested".to_string(),
        })
        .collect();
    for review in &reviews.nodes {
        let name = review
            .author
            .as_ref()
            .map_or("ghost", |user| user.login.as_str());
        if reviewers.iter().any(|existing| existing.name == name) {
            continue;
        }
        reviewers.push(Reviewer {
            name: name.to_string(),
            state: humanize(&review.state),
        });
    }
    reviewers
}

fn activity_from(comments: &Nodes<RawComment>, reviews: &Nodes<RawReview>) -> Vec<Activity> {
    let comment_items = comments.nodes.iter().map(|comment| Activity {
        author: author_name(&comment.author),
        at: comment.created_at,
        action: "commented".to_string(),
        excerpt: first_line(&comment.body),
    });
    let review_items = reviews.nodes.iter().filter_map(|review| {
        Some(Activity {
            author: author_name(&review.author),
            at: review.created_at?,
            action: humanize(&review.state),
            excerpt: first_line(&review.body),
        })
    });
    let mut items: Vec<Activity> = comment_items.chain(review_items).collect();
    items.sort_by(|left, right| right.at.cmp(&left.at));
    items
}

fn author_name(author: &Option<Login>) -> String {
    author
        .as_ref()
        .map_or_else(|| "ghost".to_string(), |user| user.login.clone())
}

fn humanize(state: &str) -> String {
    state.to_lowercase().replace('_', " ")
}

fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("<!--"))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rollup(state: &str, total: u64, runs: Value) -> Value {
        json!({ "nodes": [{ "commit": { "statusCheckRollup": {
            "state": state,
            "contexts": {
                "totalCount": total,
                "checkRunCountsByState": runs,
                "statusContextCountsByState": [{ "state": "SUCCESS", "count": 1 }],
            }
        }}}]})
    }

    fn row(number: u64, review: Value, commits: Value) -> Value {
        json!({
            "number": number,
            "title": format!("PR {number}"),
            "url": format!("https://github.com/acme/app/pull/{number}"),
            "isDraft": false,
            "updatedAt": "2026-11-15T10:00:00Z",
            "reviewDecision": review,
            "totalCommentsCount": 104,
            "author": { "login": "octo" },
            "repository": { "nameWithOwner": "acme/app" },
            "commits": commits,
        })
    }

    fn groups(titles: &[&str]) -> Vec<GroupConfig> {
        titles
            .iter()
            .map(|title| GroupConfig {
                title: title.to_string(),
                query: String::new(),
                collapsed: false,
            })
            .collect()
    }

    #[test]
    fn inbox_maps_each_alias_to_its_group() {
        let runs = json!([{ "state": "SUCCESS", "count": 3 }, { "state": "FAILURE", "count": 1 }]);
        let data = json!({
            "g0": { "issueCount": 1, "nodes": [row(7, json!("APPROVED"), rollup("FAILURE", 5, runs))] },
            "g1": { "issueCount": 120, "nodes": [] },
        });
        let parsed = parse_inbox(&data, &groups(&["Mine", "Team"])).unwrap();

        assert_eq!(parsed[0].title, "Mine");
        let pr = &parsed[0].prs[0];
        assert_eq!(pr.number, 7);
        assert_eq!(pr.review, ReviewState::Approved);
        assert_eq!(pr.comments, 104);
        // 3 passing check runs plus 1 passing status context, out of 5.
        assert_eq!(
            pr.checks,
            Some(CheckSummary {
                passed: 4,
                total: 5,
                state: CheckState::Failing
            })
        );
        assert_eq!(parsed[1].total, 120);
        assert!(parsed[1].prs.is_empty());
    }

    #[test]
    fn missing_alias_is_an_error_naming_the_field() {
        let error = parse_inbox(&json!({}), &groups(&["Mine"])).unwrap_err();
        assert!(error.to_string().contains("g0"));
    }

    #[test]
    fn null_decision_awaits_approval_and_no_rollup_means_no_checks() {
        let data = json!({ "g0": { "issueCount": 1, "nodes": [row(1, Value::Null, json!({ "nodes": [] }))] } });
        let pr = &parse_inbox(&data, &groups(&["Mine"])).unwrap()[0].prs[0];
        assert_eq!(pr.review, ReviewState::Awaiting);
        assert_eq!(pr.checks, None);
    }

    #[test]
    fn deleted_author_reads_as_ghost() {
        let mut raw = row(1, json!("REVIEW_REQUIRED"), json!({ "nodes": [] }));
        raw["author"] = Value::Null;
        let data = json!({ "g0": { "issueCount": 1, "nodes": [raw] } });
        let pr = &parse_inbox(&data, &groups(&["Mine"])).unwrap()[0].prs[0];
        assert_eq!(pr.author, "ghost");
        assert_eq!(pr.review, ReviewState::Awaiting);
    }

    fn detail_fixture() -> Value {
        let mut pr = row(
            56,
            json!("REVIEW_REQUIRED"),
            json!({ "nodes": [{ "commit": { "statusCheckRollup": {
                "state": "PENDING",
                "contexts": {
                    "totalCount": 3,
                    "checkRunCountsByState": [],
                    "statusContextCountsByState": [],
                    "nodes": [
                        { "__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS" },
                        { "__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": null },
                        { "__typename": "StatusContext", "context": "deploy", "state": "FAILURE" },
                    ]
                }
            }}}]}),
        );
        let extra = json!({
            "body": "## Reason\nBecause.",
            "createdAt": "2026-11-12T10:00:00Z",
            "additions": 55, "deletions": 0, "changedFiles": 2,
            "baseRefName": "main", "headRefName": "feature",
            "labels": { "nodes": [{ "name": "created-by-skill" }] },
            "assignees": { "nodes": [{ "login": "lead" }] },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "lead" } },
                { "requestedReviewer": { "name": "backend" } },
            ]},
            "latestOpinionatedReviews": { "nodes": [
                { "author": { "login": "lead" }, "state": "CHANGES_REQUESTED" },
                { "author": { "login": "peer" }, "state": "APPROVED" },
            ]},
            "comments": { "nodes": [
                { "author": { "login": "peer" }, "createdAt": "2026-11-13T10:00:00Z", "body": "<!-- bot -->\n\nLooks fine" },
            ]},
            "reviews": { "nodes": [
                { "author": { "login": "lead" }, "createdAt": "2026-11-14T10:00:00Z", "state": "CHANGES_REQUESTED", "body": "" },
            ]},
        });
        for (key, value) in extra.as_object().unwrap() {
            pr[key] = value.clone();
        }
        pr
    }

    #[test]
    fn detail_sorts_checks_failing_first() {
        let detail = parse_detail(&detail_fixture()).unwrap();
        let states: Vec<_> = detail
            .checks
            .iter()
            .map(|check| (check.name.as_str(), check.state))
            .collect();
        assert_eq!(
            states,
            [
                ("deploy", CheckState::Failing),
                ("test", CheckState::Pending),
                ("lint", CheckState::Passing)
            ]
        );
    }

    #[test]
    fn detail_reviewers_prefer_the_pending_request() {
        let detail = parse_detail(&detail_fixture()).unwrap();
        let reviewers: Vec<_> = detail
            .reviewers
            .iter()
            .map(|r| (r.name.as_str(), r.state.as_str()))
            .collect();
        assert_eq!(
            reviewers,
            [
                ("lead", "requested"),
                ("backend", "requested"),
                ("peer", "approved")
            ]
        );
    }

    #[test]
    fn detail_activity_is_newest_first_and_skips_html_comments() {
        let detail = parse_detail(&detail_fixture()).unwrap();
        assert_eq!(detail.activity[0].action, "changes requested");
        assert_eq!(detail.activity[1].excerpt, "Looks fine");
        assert_eq!(detail.labels, ["created-by-skill"]);
        assert_eq!(detail.summary.number, 56);
    }
}

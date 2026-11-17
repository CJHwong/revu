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

/// GitHub's GraphQL budget after the latest call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    pub remaining: u64,
    pub limit: u64,
    pub reset_at: chrono::DateTime<chrono::Utc>,
}

/// Enough to fetch and link a pull request, from an inbox row or a pasted link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRef {
    pub repo: String,
    pub number: u64,
}

impl PrRef {
    pub fn url(&self) -> String {
        format!("https://github.com/{}/pull/{}", self.repo, self.number)
    }
}

/// Accepts a pull request URL (with or without the scheme, and with any tab suffix
/// such as `/files`) or the short form `owner/repo#123`.
pub fn parse_pr_ref(text: &str) -> Option<PrRef> {
    let text = text.trim();
    let without_scheme = text
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let Some(path) = without_scheme.strip_prefix("github.com/") else {
        let (repo, number) = text.split_once('#')?;
        return pr_ref_from_parts(repo, number);
    };
    let mut parts = path.split('/');
    let (owner, name, kind, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if kind != "pull" {
        return None;
    }
    pr_ref_from_parts(&format!("{owner}/{name}"), number)
}

fn pr_ref_from_parts(repo: &str, number: &str) -> Option<PrRef> {
    let (owner, name) = repo.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    // A URL can carry a query or fragment after the number.
    let digits: String = number.chars().take_while(char::is_ascii_digit).collect();
    Some(PrRef {
        repo: repo.to_string(),
        number: digits.parse().ok()?,
    })
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

impl PrSummary {
    pub fn reference(&self) -> PrRef {
        PrRef {
            repo: self.repo.clone(),
            number: self.number,
        }
    }
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
    /// The Actions workflow that ran the check. Status contexts have none.
    pub workflow: Option<String>,
    pub url: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewedState {
    Viewed,
    Unviewed,
    /// Marked viewed once, then the file changed again.
    Dismissed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    pub path: String,
    pub additions: u64,
    pub deletions: u64,
    /// GitHub's change type in lower case: added, modified, deleted, renamed, copied, changed.
    pub change: String,
    pub viewed: ViewedState,
}

/// One page of `pullRequest.files`, plus the cursor for the next page.
pub struct FilesPage {
    pub files: Vec<ChangedFile>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitLine {
    pub oid: String,
    pub headline: String,
    /// The commit message after the headline, often empty.
    pub body: String,
    pub author: String,
    pub authored_at: DateTime<Utc>,
    pub url: String,
    pub checks: Option<CheckState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reviewer {
    pub name: String,
    /// GitHub's review state in lower case, or "requested" for a pending request.
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    /// Hex color without the `#`, as GitHub stores it.
    pub color: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    pub login: String,
    pub bot: bool,
}

/// A review comment thread, shown under the review that started it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadLine {
    pub path: String,
    /// "line 548" or "lines 827-830".
    pub lines: String,
    pub resolved: bool,
    /// The thread's own comments, oldest first.
    pub comments: Vec<ThreadComment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    /// You wrote it.
    pub mine: bool,
    pub author: Author,
    pub at: DateTime<Utc>,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub author: Author,
    pub at: DateTime<Utc>,
    /// "commented", "approved" or "requested changes".
    pub verb: String,
    pub body: String,
    pub threads: Vec<ThreadLine>,
    /// You wrote it.
    pub mine: bool,
}

/// The Overview conversation in GitHub's order, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineItem {
    Comment(Comment),
    Commit {
        oid: String,
        headline: String,
        at: DateTime<Utc>,
    },
}

/// The slower half of a pull request, fetched next to `PrDetail`.
#[derive(Debug, Clone)]
pub struct PrActivity {
    /// Failing checks first, then pending, then passing.
    pub checks: Vec<CheckLine>,
    /// Oldest first, like GitHub's Commits tab. At most 100.
    pub commits: Vec<CommitLine>,
    pub timeline: Vec<TimelineItem>,
    /// GitHub returned its limit of timeline items, so older ones are missing.
    pub timeline_cut: bool,
}

#[derive(Debug, Clone)]
pub struct PrDetail {
    /// GraphQL node id, which the viewed-file mutations take.
    pub id: String,
    pub base_oid: String,
    pub head_oid: String,
    pub summary: PrSummary,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub base: String,
    pub head: String,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub labels: Vec<Label>,
    pub assignees: Vec<String>,
    pub reviewers: Vec<Reviewer>,
    pub commit_count: u64,
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
#[serde(tag = "__typename", rename_all_fields = "camelCase")]
enum RawCheck {
    CheckRun {
        name: String,
        conclusion: Option<String>,
        status: String,
        #[serde(default)]
        details_url: Option<String>,
        #[serde(default)]
        started_at: Option<DateTime<Utc>>,
        #[serde(default)]
        completed_at: Option<DateTime<Utc>>,
        #[serde(default)]
        check_suite: Option<RawCheckSuite>,
    },
    StatusContext {
        context: String,
        state: String,
        #[serde(default)]
        target_url: Option<String>,
        #[serde(default)]
        created_at: Option<DateTime<Utc>>,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCheckSuite {
    workflow_run: Option<RawWorkflowRun>,
}

#[derive(Deserialize)]
struct RawWorkflowRun {
    workflow: Name,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommitList {
    nodes: Vec<RawCommitNode>,
}

#[derive(Deserialize)]
struct RawCommitNode {
    commit: RawCommitInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommitInfo {
    oid: String,
    /// The raw message. GitHub's messageHeadline cuts a long subject with "…".
    message: String,
    authored_date: DateTime<Utc>,
    url: String,
    author: Option<RawGitActor>,
    status_check_rollup: Option<RawRollupState>,
}

/// A commit author is a git identity; `user` is set only when it maps to a GitHub account.
#[derive(Deserialize)]
struct RawGitActor {
    name: Option<String>,
    user: Option<Login>,
}

#[derive(Deserialize)]
struct RawRollupState {
    state: String,
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
    id: String,
    base_ref_oid: String,
    head_ref_oid: String,
    commit_count: RawCount,
    body: String,
    created_at: DateTime<Utc>,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    base_ref_name: String,
    head_ref_name: String,
    labels: Nodes<RawLabel>,
    assignees: Nodes<Login>,
    review_requests: Nodes<RawRequest>,
    latest_opinionated_reviews: Nodes<RawReview>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCount {
    total_count: u64,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
    color: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawActivity {
    commits: Nodes<CommitNode>,
    commit_list: RawCommitList,
    timeline_items: Nodes<RawTimelineItem>,
    review_threads: Nodes<RawThread>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename", rename_all_fields = "camelCase")]
enum RawTimelineItem {
    IssueComment {
        author: Option<RawActor>,
        created_at: DateTime<Utc>,
        body: String,
        #[serde(default)]
        viewer_did_author: bool,
    },
    PullRequestReview {
        id: String,
        #[serde(default)]
        viewer_did_author: bool,
        author: Option<RawActor>,
        state: String,
        submitted_at: Option<DateTime<Utc>>,
        created_at: DateTime<Utc>,
        body: String,
    },
    PullRequestCommit {
        commit: RawTimelineCommit,
    },
}

#[derive(Deserialize)]
struct RawActor {
    #[serde(rename = "__typename")]
    kind: String,
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTimelineCommit {
    oid: String,
    message_headline: String,
    authored_date: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    is_resolved: bool,
    path: String,
    line: Option<u64>,
    original_line: Option<u64>,
    start_line: Option<u64>,
    original_start_line: Option<u64>,
    comments: Nodes<RawThreadComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreadComment {
    pull_request_review: Option<RawId>,
    #[serde(default)]
    viewer_did_author: bool,
    author: Option<RawActor>,
    created_at: DateTime<Utc>,
    body: String,
}

#[derive(Deserialize)]
struct RawId {
    id: String,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFiles {
    page_info: RawPageInfo,
    nodes: Vec<RawFile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    path: String,
    additions: u64,
    deletions: u64,
    change_type: String,
    viewer_viewed_state: String,
}

// ---- Conversion ----

/// `files` is `data.repository.pullRequest.files`.
pub fn parse_files_page(files: &Value) -> Result<FilesPage> {
    let raw: RawFiles = serde_json::from_value(files.clone()).context("parse changed files")?;
    let next_cursor = raw
        .page_info
        .end_cursor
        .filter(|_| raw.page_info.has_next_page);
    let files = raw
        .nodes
        .into_iter()
        .map(|file| ChangedFile {
            viewed: match file.viewer_viewed_state.as_str() {
                "VIEWED" => ViewedState::Viewed,
                "DISMISSED" => ViewedState::Dismissed,
                _ => ViewedState::Unviewed,
            },
            change: humanize(&file.change_type),
            path: file.path,
            additions: file.additions,
            deletions: file.deletions,
        })
        .collect();
    Ok(FilesPage { files, next_cursor })
}

/// `search` is one group's `data.search` object.
pub fn parse_group(search: &Value, config: &GroupConfig) -> Result<Group> {
    let search: RawSearch = serde_json::from_value(search.clone())
        .with_context(|| format!("parse group \"{}\"", config.title))?;
    Ok(Group {
        title: config.title.clone(),
        total: search.issue_count,
        prs: search.nodes.into_iter().map(summary_from).collect(),
        collapsed: config.collapsed,
    })
}

/// `pull_request` is `data.repository.pullRequest` from the detail query.
pub fn parse_detail(pull_request: &Value) -> Result<PrDetail> {
    let raw: RawDetail =
        serde_json::from_value(pull_request.clone()).context("parse pull request")?;
    let reviewers = reviewers_from(&raw.review_requests, &raw.latest_opinionated_reviews);
    let labels = raw.labels.nodes.into_iter();
    Ok(PrDetail {
        id: raw.id,
        base_oid: raw.base_ref_oid,
        head_oid: raw.head_ref_oid,
        commit_count: raw.commit_count.total_count,
        summary: summary_from(raw.row),
        body: raw.body,
        created_at: raw.created_at,
        base: raw.base_ref_name,
        head: raw.head_ref_name,
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        labels: labels
            .map(|label| Label {
                name: label.name,
                color: label.color,
            })
            .collect(),
        assignees: raw
            .assignees
            .nodes
            .into_iter()
            .map(|user| user.login)
            .collect(),
        reviewers,
    })
}

/// The `last:` count in the activity query's `timelineItems`.
pub const TIMELINE_LIMIT: usize = 100;

/// `pull_request` is `data.repository.pullRequest` from the activity query.
pub fn parse_activity(pull_request: &Value) -> Result<PrActivity> {
    let raw: RawActivity =
        serde_json::from_value(pull_request.clone()).context("parse pull request activity")?;
    let commits = raw
        .commit_list
        .nodes
        .into_iter()
        .map(|node| commit_line(node.commit))
        .collect();
    Ok(PrActivity {
        checks: check_lines(latest_rollup(&raw.commits)),
        commits,
        timeline_cut: raw.timeline_items.nodes.len() >= TIMELINE_LIMIT,
        timeline: timeline_from(raw.timeline_items.nodes, raw.review_threads.nodes),
    })
}

fn summary_from(raw: RawRow) -> PrSummary {
    let checks = latest_rollup(&raw.commits).and_then(check_summary);
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

fn latest_rollup(commits: &Nodes<CommitNode>) -> Option<&Rollup> {
    commits.nodes.last()?.commit.status_check_rollup.as_ref()
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

fn check_lines(rollup: Option<&Rollup>) -> Vec<CheckLine> {
    let Some(rollup) = rollup else {
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
            details_url,
            started_at,
            completed_at,
            check_suite,
        } => CheckLine {
            name: name.clone(),
            state: check_run_state(status, conclusion.as_deref()),
            workflow: check_suite
                .as_ref()
                .and_then(|suite| suite.workflow_run.as_ref())
                .map(|run| run.workflow.name.clone()),
            url: details_url.clone(),
            started_at: *started_at,
            completed_at: *completed_at,
        },
        RawCheck::StatusContext {
            context,
            state,
            target_url,
            created_at,
        } => CheckLine {
            name: context.clone(),
            state: rollup_state(state),
            workflow: None,
            url: target_url.clone(),
            started_at: *created_at,
            completed_at: None,
        },
    }
}

fn commit_line(commit: RawCommitInfo) -> CommitLine {
    let (headline, body) = commit
        .message
        .split_once('\n')
        .unwrap_or((&commit.message, ""));
    let author = commit
        .author
        .and_then(|actor| actor.user.map(|user| user.login).or(actor.name));
    CommitLine {
        oid: commit.oid,
        headline: headline.to_string(),
        body: body.trim().to_string(),
        author: author.unwrap_or_else(|| "unknown".to_string()),
        authored_at: commit.authored_date,
        url: commit.url,
        checks: commit
            .status_check_rollup
            .map(|rollup| rollup_state(&rollup.state)),
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

/// Comments, reviews and commits in GitHub's order. A review with no text and no
/// threads is a reply to a thread, which GitHub's Overview does not show on its own.
fn timeline_from(items: Vec<RawTimelineItem>, threads: Vec<RawThread>) -> Vec<TimelineItem> {
    let mut threads_by_review: std::collections::HashMap<String, Vec<ThreadLine>> =
        std::collections::HashMap::new();
    for thread in threads {
        let review = thread
            .comments
            .nodes
            .first()
            .and_then(|comment| comment.pull_request_review.as_ref())
            .map(|review| review.id.clone());
        if let Some(review) = review {
            threads_by_review
                .entry(review)
                .or_default()
                .push(thread_line(thread));
        }
    }
    items
        .into_iter()
        .filter_map(|item| timeline_item(item, &mut threads_by_review))
        .collect()
}

fn timeline_item(
    item: RawTimelineItem,
    threads_by_review: &mut std::collections::HashMap<String, Vec<ThreadLine>>,
) -> Option<TimelineItem> {
    let comment = match item {
        RawTimelineItem::PullRequestCommit { commit } => {
            return Some(TimelineItem::Commit {
                oid: commit.oid,
                headline: commit.message_headline,
                at: commit.authored_date,
            });
        }
        RawTimelineItem::IssueComment {
            author,
            created_at,
            body,
            viewer_did_author,
        } => Comment {
            author: author_from(author),
            at: created_at,
            verb: "commented".to_string(),
            body,
            threads: Vec::new(),
            mine: viewer_did_author,
        },
        RawTimelineItem::PullRequestReview {
            id,
            viewer_did_author,
            author,
            state,
            submitted_at,
            created_at,
            body,
        } => {
            let threads = threads_by_review.remove(&id).unwrap_or_default();
            if state == "COMMENTED" && body.trim().is_empty() && threads.is_empty() {
                return None;
            }
            Comment {
                author: author_from(author),
                at: submitted_at.unwrap_or(created_at),
                verb: review_verb(&state).to_string(),
                body,
                threads,
                mine: viewer_did_author,
            }
        }
    };
    Some(TimelineItem::Comment(comment))
}

fn author_from(actor: Option<RawActor>) -> Author {
    match actor {
        Some(actor) => Author {
            bot: actor.kind == "Bot",
            login: actor.login,
        },
        None => Author {
            login: "ghost".to_string(),
            bot: false,
        },
    }
}

fn review_verb(state: &str) -> &'static str {
    match state {
        "APPROVED" => "approved",
        "CHANGES_REQUESTED" => "requested changes",
        _ => "commented",
    }
}

/// A thread on an outdated diff has no current line, so it falls back to the original one.
fn thread_line(thread: RawThread) -> ThreadLine {
    let end = thread.line.or(thread.original_line);
    let start = thread.start_line.or(thread.original_start_line);
    let lines = match (start, end) {
        (Some(start), Some(end)) if start != end => format!("lines {start}-{end}"),
        (_, Some(end)) => format!("line {end}"),
        _ => "the file".to_string(),
    };
    let comments = thread
        .comments
        .nodes
        .into_iter()
        .map(|comment| ThreadComment {
            mine: comment.viewer_did_author,
            author: author_from(comment.author),
            at: comment.created_at,
            body: comment.body,
        });
    ThreadLine {
        path: thread.path,
        lines,
        resolved: thread.is_resolved,
        comments: comments.collect(),
    }
}

fn humanize(state: &str) -> String {
    state.to_lowercase().replace('_', " ")
}

#[cfg(test)]
pub mod tests {
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
        let configs = groups(&["Mine", "Team"]);
        let mine = json!({ "issueCount": 1, "nodes": [row(7, json!("APPROVED"), rollup("FAILURE", 5, runs))] });
        let team = json!({ "issueCount": 120, "nodes": [] });
        let parsed = [
            parse_group(&mine, &configs[0]).unwrap(),
            parse_group(&team, &configs[1]).unwrap(),
        ];

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
    fn a_bad_search_is_an_error_naming_the_group() {
        let error = parse_group(&json!({}), &groups(&["Mine"])[0]).unwrap_err();
        assert!(error.to_string().contains("Mine"));
    }

    #[test]
    fn null_decision_awaits_approval_and_no_rollup_means_no_checks() {
        let search =
            json!({ "issueCount": 1, "nodes": [row(1, Value::Null, json!({ "nodes": [] }))] });
        let pr = &parse_group(&search, &groups(&["Mine"])[0]).unwrap().prs[0];
        assert_eq!(pr.review, ReviewState::Awaiting);
        assert_eq!(pr.checks, None);
    }

    #[test]
    fn deleted_author_reads_as_ghost() {
        let mut raw = row(1, json!("REVIEW_REQUIRED"), json!({ "nodes": [] }));
        raw["author"] = Value::Null;
        let search = json!({ "issueCount": 1, "nodes": [raw] });
        let pr = &parse_group(&search, &groups(&["Mine"])[0]).unwrap().prs[0];
        assert_eq!(pr.author, "ghost");
        assert_eq!(pr.review, ReviewState::Awaiting);
    }

    pub fn sample_detail() -> PrDetail {
        parse_detail(&detail_fixture()).unwrap()
    }

    pub fn sample_activity() -> PrActivity {
        parse_activity(&activity_fixture()).unwrap()
    }

    fn detail_fixture() -> Value {
        let counts = json!({ "nodes": [{ "commit": { "statusCheckRollup": {
            "state": "PENDING",
            "contexts": { "totalCount": 3, "checkRunCountsByState": [], "statusContextCountsByState": [] }
        }}}]});
        let mut pr = row(56, json!("REVIEW_REQUIRED"), counts);
        let extra = json!({
            "id": "PR_node",
            "baseRefOid": "base000",
            "headRefOid": "head999",
            "commitCount": { "totalCount": 2 },
            "body": "## Reason\nBecause.",
            "createdAt": "2026-11-12T10:00:00Z",
            "additions": 55, "deletions": 0, "changedFiles": 2,
            "baseRefName": "main", "headRefName": "feature",
            "labels": { "nodes": [{ "name": "created-by-skill", "color": "0e8a16" }] },
            "assignees": { "nodes": [{ "login": "lead" }] },
            "reviewRequests": { "nodes": [
                { "requestedReviewer": { "login": "lead" } },
                { "requestedReviewer": { "name": "backend" } },
            ]},
            "latestOpinionatedReviews": { "nodes": [
                { "author": { "login": "lead" }, "state": "CHANGES_REQUESTED" },
                { "author": { "login": "peer" }, "state": "APPROVED" },
            ]},
        });
        for (key, value) in extra.as_object().unwrap() {
            pr[key] = value.clone();
        }
        pr
    }

    fn activity_fixture() -> Value {
        json!({
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": {
                "state": "PENDING",
                "contexts": {
                    "totalCount": 3,
                    "checkRunCountsByState": [],
                    "statusContextCountsByState": [],
                    "nodes": [
                        { "__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS",
                          "detailsUrl": "https://ci/lint", "startedAt": "2026-11-12T10:00:00Z",
                          "completedAt": "2026-11-12T10:02:30Z",
                          "checkSuite": { "workflowRun": { "workflow": { "name": "CI" } } } },
                        { "__typename": "CheckRun", "name": "test", "status": "IN_PROGRESS", "conclusion": null },
                        { "__typename": "StatusContext", "context": "deploy", "state": "FAILURE" },
                    ]
                }
            }}}]},
            "commitList": { "nodes": [
                { "commit": {
                    "oid": "aaa111", "message": "first\n\n[why] a reason\n", "authoredDate": "2026-11-12T10:00:00Z",
                    "url": "https://github.com/acme/app/commit/aaa111",
                    "author": { "name": "Octo Cat", "user": { "login": "octo" } },
                    "statusCheckRollup": { "state": "SUCCESS" } } },
                { "commit": {
                    "oid": "bbb222", "message": "second", "authoredDate": "2026-11-13T10:00:00Z",
                    "url": "https://github.com/acme/app/commit/bbb222",
                    "author": { "name": "No Account", "user": null },
                    "statusCheckRollup": null } },
            ]},
            "timelineItems": { "nodes": [
                { "__typename": "PullRequestCommit", "commit": {
                    "oid": "aaa111", "messageHeadline": "first", "authoredDate": "2026-11-12T10:00:00Z" } },
                { "__typename": "IssueComment", "author": { "__typename": "Bot", "login": "coderabbitai" },
                  "createdAt": "2026-11-13T10:00:00Z", "body": "<!-- bot -->\n\nLooks fine" },
                { "__typename": "PullRequestReview", "id": "R1", "author": { "__typename": "User", "login": "lead" },
                  "state": "CHANGES_REQUESTED", "submittedAt": "2026-11-14T10:00:00Z",
                  "createdAt": "2026-11-14T09:00:00Z", "body": "" },
                { "__typename": "PullRequestReview", "id": "R2", "author": { "__typename": "User", "login": "octo" },
                  "state": "COMMENTED", "submittedAt": null, "createdAt": "2026-11-14T11:00:00Z", "body": "" },
            ]},
            "reviewThreads": { "nodes": [
                { "isResolved": true, "path": "a.md", "line": null, "originalLine": 830,
                  "startLine": null, "originalStartLine": 827,
                  "comments": { "nodes": [{ "pullRequestReview": { "id": "R1" },
                      "author": { "__typename": "User", "login": "lead" },
                      "createdAt": "2026-11-14T10:00:00Z", "body": "Rename this." }] } },
                { "isResolved": false, "path": "b.md", "line": 12, "originalLine": 12,
                  "startLine": null, "originalStartLine": null,
                  "comments": { "nodes": [
                      { "pullRequestReview": { "id": "R1" }, "author": { "__typename": "User", "login": "lead" },
                        "createdAt": "2026-11-14T10:00:00Z", "body": "Why **here**?" },
                      { "pullRequestReview": { "id": "R9" }, "author": { "__typename": "User", "login": "octo" },
                        "createdAt": "2026-11-14T11:00:00Z", "body": "Because.", "viewerDidAuthor": true },
                  ]} },
            ]},
        })
    }

    #[test]
    fn activity_sorts_checks_failing_first() {
        let states: Vec<_> = sample_activity()
            .checks
            .iter()
            .map(|check| (check.name.clone(), check.state))
            .collect();
        let expected = [
            ("deploy", CheckState::Failing),
            ("test", CheckState::Pending),
            ("lint", CheckState::Passing),
        ];
        assert_eq!(
            states,
            expected.map(|(name, state)| (name.to_string(), state))
        );
    }

    #[test]
    fn detail_reviewers_prefer_the_pending_request() {
        let detail = sample_detail();
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
        assert_eq!(detail.labels[0].color, "0e8a16");
        assert_eq!(detail.commit_count, 2);
        assert_eq!(detail.summary.checks.map(|checks| checks.total), Some(3));
    }

    #[test]
    fn timeline_keeps_order_attaches_threads_and_drops_bare_replies() {
        let timeline = sample_activity().timeline;
        assert_eq!(
            timeline.len(),
            3,
            "the empty COMMENTED review is a thread reply"
        );
        assert!(matches!(&timeline[0], TimelineItem::Commit { oid, .. } if oid == "aaa111"));
        let TimelineItem::Comment(bot) = &timeline[1] else {
            panic!("expected a comment")
        };
        assert_eq!(
            bot.author,
            Author {
                login: "coderabbitai".to_string(),
                bot: true
            }
        );
        let TimelineItem::Comment(review) = &timeline[2] else {
            panic!("expected a review")
        };
        assert_eq!(review.verb, "requested changes");
        assert_eq!(
            review.at.to_rfc3339(),
            "2026-11-14T10:00:00+00:00",
            "uses the submit time"
        );
        let threads: Vec<_> = review
            .threads
            .iter()
            .map(|thread| (thread.path.as_str(), thread.lines.as_str(), thread.resolved))
            .collect();
        assert_eq!(
            threads,
            [("a.md", "lines 827-830", true), ("b.md", "line 12", false)]
        );
    }

    #[test]
    fn activity_carries_commits_and_check_metadata() {
        let activity = sample_activity();
        assert_eq!(activity.commits[0].headline, "first");
        assert_eq!(
            activity.commits[0].body, "[why] a reason",
            "the body comes from the raw message"
        );
        assert_eq!(activity.commits[0].author, "octo");
        assert_eq!(activity.commits[0].checks, Some(CheckState::Passing));
        assert_eq!(activity.commits[1].author, "No Account");
        assert_eq!(activity.commits[1].checks, None);
        let lint = activity
            .checks
            .iter()
            .find(|check| check.name == "lint")
            .unwrap();
        assert_eq!(lint.workflow.as_deref(), Some("CI"));
        assert_eq!(lint.url.as_deref(), Some("https://ci/lint"));
        assert!(lint.completed_at.is_some());
    }

    #[test]
    fn pr_links_parse_in_every_common_form() {
        let expected = Some(PrRef {
            repo: "acme/app".to_string(),
            number: 2302,
        });
        for text in [
            "https://github.com/acme/app/pull/2302",
            "https://github.com/acme/app/pull/2302/changes#diff-4642e8",
            "github.com/acme/app/pull/2302/files",
            "  acme/app#2302 ",
            "https://github.com/acme/app/pull/2302?w=1",
        ] {
            assert_eq!(parse_pr_ref(text), expected, "{text}");
        }
    }

    #[test]
    fn non_pr_links_are_rejected() {
        for text in [
            "https://github.com/acme/app/issues/1",
            "https://gitlab.com/a/b/pull/1",
            "app#1",
            "acme/app#x",
            "",
        ] {
            assert_eq!(parse_pr_ref(text), None, "{text}");
        }
    }

    #[test]
    fn files_page_maps_viewed_states_and_the_cursor() {
        let files = json!({
            "pageInfo": { "hasNextPage": true, "endCursor": "CUR" },
            "nodes": [
                { "path": "a.md", "additions": 1, "deletions": 2, "changeType": "MODIFIED", "viewerViewedState": "VIEWED" },
                { "path": "b.md", "additions": 0, "deletions": 0, "changeType": "RENAMED", "viewerViewedState": "DISMISSED" },
                { "path": "c.md", "additions": 3, "deletions": 0, "changeType": "ADDED", "viewerViewedState": "UNVIEWED" },
            ]
        });
        let page = parse_files_page(&files).unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("CUR"));
        let states: Vec<_> = page
            .files
            .iter()
            .map(|file| (file.change.as_str(), file.viewed))
            .collect();
        assert_eq!(
            states,
            [
                ("modified", ViewedState::Viewed),
                ("renamed", ViewedState::Dismissed),
                ("added", ViewedState::Unviewed)
            ]
        );
    }

    #[test]
    fn last_files_page_has_no_cursor() {
        let files =
            json!({ "pageInfo": { "hasNextPage": false, "endCursor": "CUR" }, "nodes": [] });
        assert!(parse_files_page(&files).unwrap().next_cursor.is_none());
    }
}

//! Domain types for the inbox, and the conversion from GitHub's GraphQL shape.

use std::collections::HashMap;

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

/// Who the PR waits on, from where you stand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Turn {
    Yours,
    Authors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    Failing,
    Pending,
    Passing,
    /// A check whose conditions did not apply. GitHub greys it out; it neither passes
    /// nor fails.
    Skipped,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    /// The head commit, so the inbox can tell a push since you last looked.
    pub head_oid: String,
    pub turn: Option<Turn>,
    /// GitHub found merge conflicts. GitHub computes this lazily, so a PR it has not
    /// checked yet reads as no conflict.
    pub conflicts: bool,
    /// GitHub's mergeStateStatus, which a change to the base branch moves without
    /// moving `updated_at`.
    pub merge_state: String,
    /// The branch this PR merges into.
    pub base: String,
    /// This PR's own branch. A PR whose base is another PR's head sits on top of it.
    pub head: String,
    /// The head lives in a fork, so its branch name can't link it to a PR here.
    pub cross_repo: bool,
    /// The repo's default branch. A PR based on any other branch may sit in a stack.
    pub default_branch: String,
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
    /// All the PR's changed files, across pages.
    pub total: usize,
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
    /// GitHub asked for this review because of CODEOWNERS.
    pub code_owner: bool,
    /// The team this member reviewed for, as GitHub's sidebar nests them.
    pub team: Option<String>,
}

/// Check runs and statuses by outcome, as GitHub's merge box groups them. A cancelled,
/// timed-out or errored run counts as failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CheckCounts {
    pub passed: u64,
    pub skipped: u64,
    pub pending: u64,
    pub failing: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedIssue {
    pub number: u64,
    pub title: String,
}

/// What others did after your latest comment, review or reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SinceCounts {
    /// Your latest comment, review or reply. None when you have none; the counts then
    /// cover the whole pull request.
    pub since: Option<DateTime<Utc>>,
    pub commits: u64,
    /// Force pushes and base branch changes.
    pub pushes: u64,
    pub review_comments: u64,
    pub comments: u64,
    pub review_requests: u64,
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
    /// GitHub's thread ID, which matches the thread to its hunk.
    pub id: String,
    pub path: String,
    /// "line 548" or "lines 827-830".
    pub lines: String,
    /// The diff line it is on now: its side and number. None when the thread is
    /// outdated, as its line is no longer in the diff.
    pub at: Option<(Side, u64)>,
    pub resolved: bool,
    /// The thread's own comments, oldest first.
    pub comments: Vec<ThreadComment>,
    /// The diff lines the thread is on, ending at the commented line. Empty until the
    /// hunks arrive, and for good when GitHub sends none.
    pub code: Vec<HunkLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkLine {
    /// The old file's number for a removed line, the new file's for the others.
    pub number: u64,
    /// '+', '-' or ' '.
    pub mark: char,
    pub text: String,
}

/// How many diff lines a thread shows, as GitHub's conversation tab does.
const THREAD_CODE_LINES: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    /// You wrote it.
    pub mine: bool,
    /// It sits in your pending review, so only you see it.
    pub pending: bool,
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

/// The slower half of a pull request, fetched next to `PrDetail`.
#[derive(Debug, Clone)]
pub struct PrActivity {
    /// Failing checks first, then pending, then passing.
    pub checks: Vec<CheckLine>,
    /// Oldest first, like GitHub's Commits tab. At most 100.
    pub commits: Vec<CommitLine>,
    /// The comments and reviews of the Overview, in GitHub's order, oldest first. The
    /// Commits tab lists the commits.
    pub timeline: Vec<Comment>,
    /// GitHub returned its limit of timeline items, so older ones are missing.
    pub timeline_cut: bool,
    pub threads_total: u64,
    pub threads_unresolved: u64,
    pub since: SinceCounts,
    /// Your pending review on GitHub, which line comments collect into until you submit.
    pub pending_review: Option<String>,
    /// The line comments in your pending review. Only you see them.
    pub drafts: Vec<Draft>,
}

/// A comment in your pending review: a new line comment, or a reply to a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// The review comment's node id, which the edit and delete mutations take.
    pub id: String,
    pub path: String,
    pub side: Side,
    pub line: u64,
    /// The first line of a range; None for one line.
    pub start_line: Option<u64>,
    pub body: String,
    /// A reply in a thread; the thread draws it, so select lines does not again.
    pub reply: bool,
}

impl Draft {
    /// "b.md line 11" or "b.md lines 9-11", for a box title.
    pub fn place(&self) -> String {
        match self.start_line {
            Some(start) => format!("{} lines {start}-{}", self.path, self.line),
            None => format!("{} line {}", self.path, self.line),
        }
    }
}

/// Which side of the diff a line comment is on: the base (LEFT) or the head (RIGHT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn graphql(self) -> &'static str {
        match self {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        }
    }
}

/// The verdict a submitted review carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Comment,
    Approve,
    RequestChanges,
}

impl Verdict {
    pub fn graphql(self) -> &'static str {
        match self {
            Verdict::Comment => "COMMENT",
            Verdict::Approve => "APPROVE",
            Verdict::RequestChanges => "REQUEST_CHANGES",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Verdict::Comment => "Comment",
            Verdict::Approve => "Approve",
            Verdict::RequestChanges => "Request changes",
        }
    }
}

/// How GitHub merges a PR into its base, in the order GitHub lists them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub const ALL: [MergeMethod; 3] =
        [MergeMethod::Merge, MergeMethod::Squash, MergeMethod::Rebase];

    pub fn graphql(self) -> &'static str {
        match self {
            MergeMethod::Merge => "MERGE",
            MergeMethod::Squash => "SQUASH",
            MergeMethod::Rebase => "REBASE",
        }
    }

    fn from_graphql(name: &str) -> Option<MergeMethod> {
        MergeMethod::ALL
            .into_iter()
            .find(|method| method.graphql() == name)
    }

    /// GitHub's name for the method in its merge menu.
    pub fn title(self) -> &'static str {
        match self {
            MergeMethod::Merge => "Create a merge commit",
            MergeMethod::Squash => "Squash and merge",
            MergeMethod::Rebase => "Rebase and merge",
        }
    }
}

/// A merge write to GitHub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRequest {
    /// Merge now. Past branch protection too, when you are an admin.
    Now(MergeMethod),
    /// Let GitHub merge once the requirements pass.
    Auto(MergeMethod),
    /// Switch the method auto-merge will use. GitHub keeps the old method when asked to
    /// turn it on again, so this turns it off, then on with the new one.
    ChangeAuto(MergeMethod),
    DisableAuto,
}

/// What the repo and GitHub allow you to do to merge this PR.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeOptions {
    /// The methods the repo allows, in GitHub's order.
    pub methods: Vec<MergeMethod>,
    /// The method GitHub checks first for you: your last one, or the repo's.
    pub default_method: Option<MergeMethod>,
    /// You can push to the repo, which GitHub requires to merge.
    pub can_write: bool,
    pub can_enable_auto_merge: bool,
    pub can_disable_auto_merge: bool,
    /// You can merge past branch protection, as an admin.
    pub can_bypass: bool,
    /// You can edit the PR, which GitHub requires to switch it to draft or ready.
    pub can_update: bool,
    pub auto_merge: Option<AutoMerge>,
}

/// Auto-merge, when it is on: the method it will use and who turned it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMerge {
    pub method: MergeMethod,
    pub by: Option<String>,
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
    /// GitHub's mergeStateStatus: CLEAN, UNSTABLE, BLOCKED, DIRTY, BEHIND, DRAFT, ...
    pub merge_state: String,
    pub check_counts: Option<CheckCounts>,
    pub milestone: Option<String>,
    pub linked_issues: Vec<LinkedIssue>,
    pub lifecycle: Lifecycle,
    /// You opened this PR, so GitHub refuses your Approve and Request changes.
    pub viewer_is_author: bool,
    pub merge: MergeOptions,
}

/// Where the PR is in its life, as GitHub's state pill shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lifecycle {
    Open,
    Draft,
    Merged {
        by: Option<String>,
        at: Option<DateTime<Utc>>,
    },
    Closed {
        at: Option<DateTime<Utc>>,
    },
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
    default_branch_ref: Option<RawBranch>,
}

#[derive(Deserialize)]
struct RawBranch {
    name: String,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

impl<T> Default for Nodes<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
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
    /// A rebase moves this date and leaves the authored date alone.
    committed_date: Option<DateTime<Utc>>,
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
    #[serde(default)]
    additions: u64,
    #[serde(default)]
    deletions: u64,
    #[serde(default)]
    changed_files: u64,
    #[serde(default)]
    head_ref_oid: String,
    #[serde(default)]
    base_ref_name: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    viewer_did_author: bool,
    mergeable: Option<String>,
    merge_state_status: Option<String>,
    viewer_latest_review: Option<RawReview>,
    /// Each reviewer's latest approval or change request.
    #[serde(default)]
    latest_opinionated_reviews: Nodes<RawReview>,
    /// A pending request to you or to a team you are in.
    viewer_latest_review_request: Option<RawViewerRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawViewerRequest {
    requested_reviewer: Option<RawTypename>,
}

#[derive(Deserialize)]
struct RawTypename {
    #[serde(rename = "__typename")]
    typename: String,
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
    commit_count: RawCount,
    body: String,
    created_at: DateTime<Utc>,
    labels: Nodes<RawLabel>,
    assignees: Nodes<Login>,
    review_requests: Nodes<RawRequest>,
    latest_reviews: Option<Nodes<RawLatestReview>>,
    milestone: Option<RawTitle>,
    closing_issues_references: Option<Nodes<RawIssue>>,
    state: Option<String>,
    merged_at: Option<DateTime<Utc>>,
    merged_by: Option<Login>,
    closed_at: Option<DateTime<Utc>>,
    base_repository: Option<RawMergeRepo>,
    #[serde(default)]
    viewer_can_merge_as_admin: bool,
    #[serde(default)]
    viewer_can_enable_auto_merge: bool,
    #[serde(default)]
    viewer_can_disable_auto_merge: bool,
    #[serde(default)]
    viewer_can_update: bool,
    auto_merge_request: Option<RawAutoMerge>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMergeRepo {
    viewer_permission: Option<String>,
    merge_commit_allowed: bool,
    squash_merge_allowed: bool,
    rebase_merge_allowed: bool,
    viewer_default_merge_method: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAutoMerge {
    merge_method: String,
    enabled_by: Option<Login>,
}

#[derive(Deserialize)]
struct RawTitle {
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLatestReview {
    author: Option<Login>,
    state: String,
    on_behalf_of: Nodes<Name>,
}

#[derive(Deserialize)]
struct RawIssue {
    number: u64,
    title: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEvent {
    #[serde(rename = "__typename")]
    kind: String,
    created_at: DateTime<Utc>,
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
    events: Option<Nodes<RawEvent>>,
    pending_reviews: Option<Nodes<RawId>>,
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
}

#[derive(Deserialize)]
struct RawActor {
    #[serde(rename = "__typename")]
    kind: String,
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    #[serde(default)]
    id: String,
    is_resolved: bool,
    path: String,
    line: Option<u64>,
    original_line: Option<u64>,
    start_line: Option<u64>,
    original_start_line: Option<u64>,
    #[serde(default)]
    diff_side: Option<String>,
    comments: Nodes<RawThreadComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHunks {
    review_threads: Nodes<RawHunkThread>,
}

#[derive(Deserialize)]
struct RawHunkThread {
    id: String,
    /// Only the first comment; every reply carries the same hunk.
    comments: Nodes<RawHunk>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHunk {
    diff_hunk: String,
}

/// `pull_request` is `data.repository.pullRequest` from the hunks query: each thread's
/// code by thread ID. A thread without a hunk is left out.
pub fn parse_hunks(pull_request: &Value) -> Result<HashMap<String, Vec<HunkLine>>> {
    let raw: RawHunks =
        serde_json::from_value(pull_request.clone()).context("parse review thread hunks")?;
    Ok(raw
        .review_threads
        .nodes
        .into_iter()
        .filter_map(|thread| {
            let hunk = thread.comments.nodes.into_iter().next()?.diff_hunk;
            Some((thread.id, hunk_tail(&hunk, THREAD_CODE_LINES)))
        })
        .collect())
}

impl PrActivity {
    /// The review threads on one file, in timeline order.
    pub fn threads_on<'a>(&'a self, path: &'a str) -> impl Iterator<Item = &'a ThreadLine> {
        self.timeline
            .iter()
            .flat_map(|comment| &comment.threads)
            .filter(move |thread| thread.path == path)
    }

    /// Puts the hunks from `parse_hunks` under their threads.
    pub fn set_code(&mut self, hunks: &HashMap<String, Vec<HunkLine>>) {
        let threads = self
            .timeline
            .iter_mut()
            .flat_map(|comment| &mut comment.threads);
        for thread in threads {
            if let Some(code) = hunks.get(&thread.id) {
                thread.code = code.clone();
            }
        }
    }
}

impl RawThread {
    /// GitHub lists your pending review's threads with the others, but only you see them.
    fn is_pending(&self) -> bool {
        self.comments
            .nodes
            .first()
            .is_some_and(RawThreadComment::is_pending)
    }
}

impl RawThreadComment {
    fn is_pending(&self) -> bool {
        let review = self.pull_request_review.as_ref();
        review.and_then(|review| review.state.as_deref()) == Some("PENDING")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThreadComment {
    #[serde(default)]
    id: String,
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
    #[serde(default)]
    state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequest {
    #[serde(default)]
    as_code_owner: bool,
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
    #[serde(default)]
    viewer_did_author: bool,
    /// The PR's head when the review was submitted.
    commit: Option<RawOid>,
}

#[derive(Deserialize)]
struct RawOid {
    oid: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFiles {
    #[serde(default)]
    total_count: usize,
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
    let total = raw.total_count;
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
    Ok(FilesPage {
        files,
        next_cursor,
        total,
    })
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
    let merge = merge_options(&raw);
    let reviewers = reviewers_from(
        &raw.review_requests,
        &raw.row.latest_opinionated_reviews,
        raw.latest_reviews.as_ref(),
    );
    let labels = raw.labels.nodes.into_iter();
    let check_counts = latest_rollup(&raw.row.commits).map(check_counts);
    let linked_issues = raw
        .closing_issues_references
        .map_or_else(Vec::new, |issues| {
            let issues = issues.nodes.into_iter();
            let linked = issues.map(|issue| LinkedIssue {
                number: issue.number,
                title: issue.title,
            });
            linked.collect()
        });
    let lifecycle = match raw.state.as_deref() {
        Some("MERGED") => Lifecycle::Merged {
            by: raw.merged_by.map(|actor| actor.login),
            at: raw.merged_at,
        },
        Some("CLOSED") => Lifecycle::Closed { at: raw.closed_at },
        _ if raw.row.is_draft => Lifecycle::Draft,
        _ => Lifecycle::Open,
    };
    Ok(PrDetail {
        merge,
        viewer_is_author: raw.row.viewer_did_author,
        lifecycle,
        merge_state: raw.row.merge_state_status.clone().unwrap_or_default(),
        check_counts,
        milestone: raw.milestone.map(|milestone| milestone.title),
        linked_issues,
        id: raw.id,
        base_oid: raw.base_ref_oid,
        head_oid: raw.row.head_ref_oid.clone(),
        commit_count: raw.commit_count.total_count,
        additions: raw.row.additions,
        deletions: raw.row.deletions,
        changed_files: raw.row.changed_files,
        base: raw.row.base_ref_name.clone(),
        head: raw.row.head_ref_name.clone(),
        summary: summary_from(raw.row),
        body: raw.body,
        created_at: raw.created_at,
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

fn merge_options(raw: &RawDetail) -> MergeOptions {
    let Some(repo) = &raw.base_repository else {
        return MergeOptions::default();
    };
    let allowed = [
        repo.merge_commit_allowed,
        repo.squash_merge_allowed,
        repo.rebase_merge_allowed,
    ];
    let methods = MergeMethod::ALL
        .into_iter()
        .zip(allowed)
        .filter_map(|(method, allowed)| allowed.then_some(method))
        .collect();
    let permission = repo.viewer_permission.as_deref();
    MergeOptions {
        methods,
        default_method: repo
            .viewer_default_merge_method
            .as_deref()
            .and_then(MergeMethod::from_graphql),
        can_write: matches!(permission, Some("WRITE" | "MAINTAIN" | "ADMIN")),
        can_enable_auto_merge: raw.viewer_can_enable_auto_merge,
        can_disable_auto_merge: raw.viewer_can_disable_auto_merge,
        can_bypass: raw.viewer_can_merge_as_admin,
        can_update: raw.viewer_can_update,
        auto_merge: raw.auto_merge_request.as_ref().and_then(|request| {
            Some(AutoMerge {
                method: MergeMethod::from_graphql(&request.merge_method)?,
                by: request.enabled_by.as_ref().map(|by| by.login.clone()),
            })
        }),
    }
}

/// The `last:` count in the activity query's `timelineItems`.
pub const TIMELINE_LIMIT: usize = 100;

/// `pull_request` is `data.repository.pullRequest` from the activity query.
pub fn parse_activity(pull_request: &Value) -> Result<PrActivity> {
    let raw: RawActivity =
        serde_json::from_value(pull_request.clone()).context("parse pull request activity")?;
    let since = since_counts(&raw);
    let (pending, threads): (Vec<RawThread>, Vec<RawThread>) = raw
        .review_threads
        .nodes
        .into_iter()
        .partition(RawThread::is_pending);
    let threads_total = threads.len() as u64;
    let threads_unresolved = threads.iter().filter(|thread| !thread.is_resolved).count() as u64;
    let pending_review = raw
        .pending_reviews
        .and_then(|reviews| reviews.nodes.into_iter().next())
        .map(|review| review.id);
    let drafts = pending
        .iter()
        .chain(&threads)
        .flat_map(thread_drafts)
        .collect();
    let commits = raw
        .commit_list
        .nodes
        .into_iter()
        .map(|node| commit_line(node.commit))
        .collect();
    Ok(PrActivity {
        pending_review,
        drafts,
        threads_total,
        threads_unresolved,
        since,
        checks: check_lines(latest_rollup(&raw.commits)),
        commits,
        timeline_cut: raw.timeline_items.nodes.len() >= TIMELINE_LIMIT,
        timeline: timeline_from(raw.timeline_items.nodes, threads),
    })
}

/// The thread's comments in your pending review. A thread on a line no longer in the
/// diff has none to show, as GitHub's own page drops them too.
fn thread_drafts(thread: &RawThread) -> Vec<Draft> {
    let Some(line) = thread.line else {
        return Vec::new();
    };
    let side = match thread.diff_side.as_deref() {
        Some("LEFT") => Side::Left,
        _ => Side::Right,
    };
    let new_thread = thread.is_pending();
    let pending = thread.comments.nodes.iter().enumerate();
    pending
        .filter(|(_, comment)| comment.is_pending())
        .map(|(index, comment)| Draft {
            id: comment.id.clone(),
            path: thread.path.clone(),
            side,
            line,
            start_line: thread.start_line.filter(|start| *start != line),
            body: comment.body.clone(),
            reply: !new_thread || index > 0,
        })
        .collect()
}

fn summary_from(raw: RawRow) -> PrSummary {
    let checks = latest_rollup(&raw.commits).and_then(check_summary);
    let conflicts = raw.mergeable.as_deref() == Some("CONFLICTING");
    let turn = turn(&raw, conflicts);
    PrSummary {
        turn,
        conflicts,
        merge_state: raw.merge_state_status.clone().unwrap_or_default(),
        default_branch: raw
            .repository
            .default_branch_ref
            .map(|branch| branch.name)
            .unwrap_or_default(),
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
        additions: raw.additions,
        deletions: raw.deletions,
        changed_files: raw.changed_files,
        head_oid: raw.head_ref_oid,
        base: raw.base_ref_name,
        head: raw.head_ref_name,
        cross_repo: raw.is_cross_repository,
    }
}

/// Your turn: a review is requested from you by name, a push came after your
/// review, or on your own PR, changes were requested or it conflicts. The author's turn:
/// you requested changes and nothing was pushed since. Your review is your latest
/// approval or change request, so a reply in a thread later does not hide it; else
/// your latest review of any kind.
fn turn(raw: &RawRow, conflicts: bool) -> Option<Turn> {
    if raw.viewer_did_author {
        let changes = raw.review_decision.as_deref() == Some("CHANGES_REQUESTED");
        return (changes || conflicts).then_some(Turn::Yours);
    }
    // A team's request is the group's to pick up, so only one by name counts.
    let by_name = raw
        .viewer_latest_review_request
        .as_ref()
        .and_then(|request| request.requested_reviewer.as_ref())
        .is_some_and(|reviewer| reviewer.typename == "User");
    if by_name {
        return Some(Turn::Yours);
    }
    let opinion = raw
        .latest_opinionated_reviews
        .nodes
        .iter()
        .find(|review| review.viewer_did_author);
    let latest = raw
        .viewer_latest_review
        .as_ref()
        .filter(|review| review.state != "PENDING");
    let review = opinion.or(latest)?;
    // The review's commit, not a date: a commit's date is when it was made, not pushed.
    let pushed_since = review
        .commit
        .as_ref()
        .is_some_and(|commit| !raw.head_ref_oid.is_empty() && commit.oid != raw.head_ref_oid);
    match review.state.as_str() {
        _ if pushed_since => Some(Turn::Yours),
        "CHANGES_REQUESTED" => Some(Turn::Authors),
        _ => None,
    }
}

fn login_or_ghost(author: Option<Login>) -> String {
    // GitHub returns a null author for deleted accounts and shows them as "ghost".
    author.map_or_else(|| "ghost".to_string(), |user| user.login)
}

/// GitHub's web inbox shows "Awaiting approval" for PRs whose API decision is null,
/// so a null decision maps to the same state.
pub fn review_state(decision: Option<&str>) -> ReviewState {
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
    // Skipped checks count neither way, so a PR with many of them does not look greener.
    let passed_runs = count_states(&contexts.check_run_counts_by_state, &["SUCCESS", "NEUTRAL"]);
    let skipped = count_states(&contexts.check_run_counts_by_state, &["SKIPPED"]);
    let passed_statuses = count_states(&contexts.status_context_counts_by_state, &["SUCCESS"]);
    Some(CheckSummary {
        passed: passed_runs + passed_statuses,
        total: contexts.total_count.saturating_sub(skipped),
        state: rollup_state(&rollup.state),
    })
}

fn check_counts(rollup: &Rollup) -> CheckCounts {
    let contexts = &rollup.contexts;
    let mut counts = CheckCounts::default();
    for entry in &contexts.check_run_counts_by_state {
        let bucket = match entry.state.as_str() {
            "SUCCESS" => &mut counts.passed,
            "NEUTRAL" | "SKIPPED" => &mut counts.skipped,
            "QUEUED" | "IN_PROGRESS" | "PENDING" | "WAITING" | "REQUESTED" => &mut counts.pending,
            _ => &mut counts.failing,
        };
        *bucket += entry.count;
    }
    for entry in &contexts.status_context_counts_by_state {
        let bucket = match entry.state.as_str() {
            "SUCCESS" => &mut counts.passed,
            "PENDING" | "EXPECTED" => &mut counts.pending,
            _ => &mut counts.failing,
        };
        *bucket += entry.count;
    }
    counts
}

/// The times of your own comments, reviews and replies.
fn my_times(raw: &RawActivity) -> Vec<DateTime<Utc>> {
    let threads = raw.review_threads.nodes.iter();
    let replies = threads.flat_map(|thread| thread.comments.nodes.iter());
    let mut times: Vec<DateTime<Utc>> = replies
        .filter(|comment| comment.viewer_did_author)
        .map(|comment| comment.created_at)
        .collect();
    for item in &raw.timeline_items.nodes {
        match item {
            RawTimelineItem::IssueComment {
                created_at,
                viewer_did_author: true,
                ..
            } => times.push(*created_at),
            RawTimelineItem::PullRequestReview {
                viewer_did_author: true,
                submitted_at,
                created_at,
                ..
            } => times.push(submitted_at.unwrap_or(*created_at)),
            _ => {}
        }
    }
    times
}

/// Counts what others did after your latest comment, review or reply.
fn since_counts(raw: &RawActivity) -> SinceCounts {
    let since = my_times(raw).into_iter().max();
    let count = |times: Vec<DateTime<Utc>>| {
        let after = times
            .into_iter()
            .filter(|at| since.is_none_or(|since| *at > since));
        after.count() as u64
    };
    let events = raw
        .events
        .as_ref()
        .map_or(&[][..], |events| events.nodes.as_slice());
    let events_of = |kinds: &[&str]| {
        let matching = events
            .iter()
            .filter(|event| kinds.contains(&event.kind.as_str()));
        count(matching.map(|event| event.created_at).collect())
    };
    let commits = raw.commit_list.nodes.iter().map(|node| {
        node.commit
            .committed_date
            .unwrap_or(node.commit.authored_date)
    });
    // GitHub counts review submissions here; a reply in a thread is one too.
    let reviews = raw
        .timeline_items
        .nodes
        .iter()
        .filter_map(|item| match item {
            RawTimelineItem::PullRequestReview {
                viewer_did_author: false,
                submitted_at,
                created_at,
                ..
            } => Some(submitted_at.unwrap_or(*created_at)),
            _ => None,
        });
    let comments = raw
        .timeline_items
        .nodes
        .iter()
        .filter_map(|item| match item {
            RawTimelineItem::IssueComment {
                created_at,
                viewer_did_author: false,
                ..
            } => Some(*created_at),
            _ => None,
        });
    SinceCounts {
        since,
        commits: count(commits.collect()),
        pushes: events_of(&[
            "HeadRefForcePushedEvent",
            "BaseRefChangedEvent",
            "BaseRefForcePushedEvent",
        ]),
        review_comments: count(reviews.collect()),
        comments: count(comments.collect()),
        review_requests: events_of(&["ReviewRequestedEvent"]),
    }
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
        Some("SUCCESS" | "NEUTRAL") => CheckState::Passing,
        Some("SKIPPED") => CheckState::Skipped,
        _ => CheckState::Failing,
    }
}

/// Pending requests come first. A review by someone who is requested again is dropped,
/// because the new request is the state that needs action.
/// Pending requests first, then everyone who reviewed. An approval or a change request
/// outranks a later plain comment, as on GitHub; `latest` adds plain comment reviews and
/// the team a member reviewed for.
fn reviewers_from(
    requests: &Nodes<RawRequest>,
    opinionated: &Nodes<RawReview>,
    latest: Option<&Nodes<RawLatestReview>>,
) -> Vec<Reviewer> {
    let mut reviewers: Vec<Reviewer> = requests
        .nodes
        .iter()
        .filter_map(|request| {
            let reviewer = request.requested_reviewer.as_ref()?;
            let name = reviewer.login.clone().or_else(|| reviewer.name.clone())?;
            Some(Reviewer {
                name,
                state: "requested".to_string(),
                code_owner: request.as_code_owner,
                team: None,
            })
        })
        .collect();
    let opinion_of = |name: &str| {
        let review = opinionated
            .nodes
            .iter()
            .find(|review| review_author(&review.author) == name);
        review.map(|review| humanize(&review.state))
    };
    let latest = latest.map_or(&[][..], |latest| latest.nodes.as_slice());
    for review in latest {
        let name = review_author(&review.author);
        if reviewers.iter().any(|existing| existing.name == name) {
            continue;
        }
        reviewers.push(Reviewer {
            name: name.to_string(),
            state: opinion_of(name).unwrap_or_else(|| humanize(&review.state)),
            code_owner: false,
            team: review
                .on_behalf_of
                .nodes
                .first()
                .map(|team| team.name.clone()),
        });
    }
    for review in &opinionated.nodes {
        let name = review_author(&review.author);
        if reviewers.iter().any(|existing| existing.name == name) {
            continue;
        }
        reviewers.push(Reviewer {
            name: name.to_string(),
            state: humanize(&review.state),
            code_owner: false,
            team: None,
        });
    }
    reviewers
}

fn review_author(author: &Option<Login>) -> &str {
    author.as_ref().map_or("ghost", |user| user.login.as_str())
}

/// Comments, reviews and commits in GitHub's order. A review with no text and no
/// threads is a reply to a thread, which GitHub's Overview does not show on its own.
fn timeline_from(items: Vec<RawTimelineItem>, threads: Vec<RawThread>) -> Vec<Comment> {
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
) -> Option<Comment> {
    let comment = match item {
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
            // A pending review is a draft, not part of the conversation yet.
            if state == "PENDING" {
                return None;
            }
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
    Some(comment)
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
    let side = match thread.diff_side.as_deref() {
        Some("LEFT") => Side::Left,
        _ => Side::Right,
    };
    let at = thread.line.map(|line| (side, line));
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
            pending: comment.is_pending(),
            mine: comment.viewer_did_author,
            author: author_from(comment.author),
            at: comment.created_at,
            body: comment.body,
        });
    ThreadLine {
        id: thread.id,
        path: thread.path,
        lines,
        at,
        resolved: thread.is_resolved,
        comments: comments.collect(),
        code: Vec::new(),
    }
}

/// The last `keep` lines of a diff hunk, numbered from its `@@ -old +new @@` header.
fn hunk_tail(hunk: &str, keep: usize) -> Vec<HunkLine> {
    let mut lines = hunk.lines();
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let start = |sign: char| {
        header
            .split_whitespace()
            .find_map(|part| part.strip_prefix(sign))
            .and_then(|range| range.split(',').next()?.parse::<u64>().ok())
            .unwrap_or(1)
    };
    let (mut old, mut new) = (start('-'), start('+'));
    let mut code = Vec::new();
    for line in lines {
        let mut chars = line.chars();
        let mark = chars.next().unwrap_or(' ');
        let number = match mark {
            '-' => {
                old += 1;
                old - 1
            }
            '+' => {
                new += 1;
                new - 1
            }
            // "\ No newline at end of file" is a note, not a line.
            '\\' => continue,
            _ => {
                old += 1;
                new += 1;
                new - 1
            }
        };
        let mark = if matches!(mark, '-' | '+') { mark } else { ' ' };
        let text = chars.as_str().replace('\t', "    ");
        code.push(HunkLine { number, mark, text });
    }
    let skip = code.len().saturating_sub(keep);
    code.split_off(skip)
}

fn humanize(state: &str) -> String {
    state.to_lowercase().replace('_', " ")
}

#[cfg(test)]
pub mod tests {
    #[test]
    fn hunk_tail_keeps_the_last_lines_with_their_numbers() {
        let hunk = "@@ -8,5 +8,6 @@ intro\n eight\n nine\n-old\tten\n+new ten\n+new eleven\n\\ No newline at end of file\n twelve";
        let line = |number, mark, text: &str| HunkLine {
            number,
            mark,
            text: text.to_string(),
        };
        assert_eq!(
            hunk_tail(hunk, 4),
            [
                line(10, '-', "old    ten"),
                line(10, '+', "new ten"),
                line(11, '+', "new eleven"),
                line(12, ' ', "twelve"),
            ],
            "a removed line keeps its old number, tabs become spaces, the no-newline note goes"
        );
        assert!(hunk_tail("", 4).is_empty());
        assert_eq!(hunk_tail(hunk, 9).len(), 6, "a short hunk shows whole");
    }

    #[test]
    fn hunks_arrive_after_the_threads_and_fill_in_their_code() {
        let mut activity = parse_activity(&activity_fixture()).unwrap();
        let code = |activity: &PrActivity, path: &str| {
            activity
                .timeline
                .iter()
                .flat_map(|comment| &comment.threads)
                .find(|thread| thread.path == path)
                .map(|thread| thread.code.clone())
                .unwrap()
        };
        assert!(
            code(&activity, "b.md").is_empty(),
            "the threads load without code"
        );
        activity.set_code(&parse_hunks(&hunks_fixture()).unwrap());
        let open = code(&activity, "b.md");
        assert_eq!(open.len(), 4);
        assert_eq!(open.last().map(|line| line.text.as_str()), Some("twelve"));
        assert!(
            code(&activity, "a.md").is_empty(),
            "no hunk sent, no code shown"
        );
    }
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
            "additions": 12, "deletions": 3, "changedFiles": 2,
            "headRefOid": "head",
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
        assert_eq!((pr.additions, pr.deletions, pr.changed_files), (12, 3, 2));
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
    fn a_row_says_whose_turn_it_is() {
        // Reviews name the head they were made on; the row's head is "head".
        let review = |state: &str, on: &str| json!({ "state": state, "commit": { "oid": on } });
        let opinion = |state: &str, on: &str, mine: bool| json!({ "nodes": [{ "state": state, "viewerDidAuthor": mine, "commit": { "oid": on } }] });
        let turn = |fields: Value| {
            let mut raw = row(1, json!("REVIEW_REQUIRED"), json!({ "nodes": [] }));
            raw.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let search = json!({ "issueCount": 1, "nodes": [raw] });
            let pr = parse_group(&search, &groups(&["Mine"])[0])
                .unwrap()
                .prs
                .remove(0);
            (pr.turn, pr.conflicts)
        };
        let yours = Some(Turn::Yours);
        let authors = Some(Turn::Authors);
        assert_eq!(turn(json!({})), (None, false), "nothing asked of you");
        let requested = |typename: &str| json!({ "viewerLatestReviewRequest": { "requestedReviewer": { "__typename": typename } } });
        assert_eq!(
            turn(requested("User")),
            (yours, false),
            "a request to you by name"
        );
        assert_eq!(turn(requested("Team")), (None, false), "a team's request");
        assert_eq!(
            turn(json!({ "viewerLatestReview": review("CHANGES_REQUESTED", "head") })),
            (authors, false),
            "you asked for changes and nothing came since"
        );
        assert_eq!(
            turn(json!({ "viewerLatestReview": review("COMMENTED", "older") })),
            (yours, false),
            "a push after your review, however old its commits' dates"
        );
        assert_eq!(
            turn(json!({ "viewerLatestReview": review("APPROVED", "head") })),
            (None, false)
        );
        assert_eq!(
            turn(json!({
                "latestOpinionatedReviews": opinion("CHANGES_REQUESTED", "head", true),
                "viewerLatestReview": review("COMMENTED", "head"),
            })),
            (authors, false),
            "a reply in a thread after your change request"
        );
        assert_eq!(
            turn(json!({
                "latestOpinionatedReviews": opinion("CHANGES_REQUESTED", "older", true),
                "viewerLatestReview": review("COMMENTED", "head"),
            })),
            (yours, false),
            "a push after your change request, though you commented since"
        );
        assert_eq!(
            turn(json!({
                "latestOpinionatedReviews": opinion("CHANGES_REQUESTED", "head", false),
            })),
            (None, false),
            "someone else's change request"
        );
        assert_eq!(
            turn(json!({ "viewerDidAuthor": true, "reviewDecision": "CHANGES_REQUESTED" })),
            (yours, false),
            "changes requested on your PR"
        );
        assert_eq!(
            turn(json!({ "viewerDidAuthor": true, "mergeable": "CONFLICTING" })),
            (yours, true),
            "your PR conflicts"
        );
        assert_eq!(
            turn(json!({ "viewerDidAuthor": true, "mergeable": "UNKNOWN" })),
            (None, false),
            "GitHub has not checked yet"
        );
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
        let mut activity = parse_activity(&activity_fixture()).unwrap();
        activity.set_code(&parse_hunks(&hunks_fixture()).unwrap());
        activity
    }

    /// The hunks query's answer for the threads in `activity_fixture`. T1 has no hunk.
    fn hunks_fixture() -> Value {
        json!({ "reviewThreads": { "nodes": [
            { "id": "T1", "comments": { "nodes": [] } },
            { "id": "T2", "comments": { "nodes": [{ "diffHunk":
                "@@ -8,5 +8,6 @@ intro\n eight\n nine\n-old ten\n+new ten\n+new eleven\n twelve" }] } },
        ]}})
    }

    #[test]
    fn merge_options_read_the_repo_settings_and_your_rights() {
        let mut detail = detail_fixture();
        assert_eq!(
            parse_detail(&detail).unwrap().merge,
            MergeOptions::default(),
            "no repo settings, nothing offered"
        );
        detail["baseRepository"] = json!({
            "viewerPermission": "MAINTAIN",
            "mergeCommitAllowed": false,
            "squashMergeAllowed": true,
            "rebaseMergeAllowed": true,
            "viewerDefaultMergeMethod": "REBASE",
        });
        detail["viewerCanMergeAsAdmin"] = json!(true);
        detail["viewerCanDisableAutoMerge"] = json!(true);
        detail["autoMergeRequest"] =
            json!({ "mergeMethod": "SQUASH", "enabledBy": { "login": "octo" } });
        detail["viewerCanUpdate"] = json!(true);
        let merge = parse_detail(&detail).unwrap().merge;
        assert_eq!(
            merge,
            MergeOptions {
                methods: vec![MergeMethod::Squash, MergeMethod::Rebase],
                default_method: Some(MergeMethod::Rebase),
                can_write: true,
                can_enable_auto_merge: false,
                can_disable_auto_merge: true,
                can_bypass: true,
                can_update: true,
                auto_merge: Some(AutoMerge {
                    method: MergeMethod::Squash,
                    by: Some("octo".to_string()),
                }),
            }
        );
        detail["baseRepository"]["viewerPermission"] = json!("READ");
        assert!(!parse_detail(&detail).unwrap().merge.can_write);
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

    pub fn activity_fixture() -> Value {
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
                { "__typename": "IssueComment", "author": { "__typename": "Bot", "login": "coderabbitai" },
                  "createdAt": "2026-11-13T10:00:00Z", "body": "<!-- bot -->\n\nLooks fine" },
                { "__typename": "PullRequestReview", "id": "R1", "author": { "__typename": "User", "login": "lead" },
                  "state": "CHANGES_REQUESTED", "submittedAt": "2026-11-14T10:00:00Z",
                  "createdAt": "2026-11-14T09:00:00Z", "body": "" },
                { "__typename": "PullRequestReview", "id": "R2", "author": { "__typename": "User", "login": "octo" },
                  "state": "COMMENTED", "submittedAt": null, "createdAt": "2026-11-14T11:00:00Z", "body": "" },
            ]},
            "reviewThreads": { "nodes": [
                { "id": "T1", "isResolved": true, "path": "a.md", "line": null, "originalLine": 830,
                  "startLine": null, "originalStartLine": 827,
                  "comments": { "nodes": [{ "pullRequestReview": { "id": "R1" },
                      "author": { "__typename": "User", "login": "lead" },
                      "createdAt": "2026-11-14T10:00:00Z", "body": "Rename this." }] } },
                { "id": "T2", "isResolved": false, "path": "b.md", "line": 12, "originalLine": 12,
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
    fn a_thread_sits_on_its_current_line_or_on_none_when_outdated() {
        let activity = sample_activity();
        let at = |path: &str| {
            activity
                .threads_on(path)
                .map(|thread| thread.at)
                .collect::<Vec<_>>()
        };
        assert_eq!(at("a.md"), [None], "outdated: GitHub sends no line");
        assert_eq!(at("b.md"), [Some((Side::Right, 12))]);
    }

    #[test]
    fn cancelled_and_errored_checks_count_as_failing() {
        let mut detail = detail_fixture();
        detail["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"] = json!({
            "totalCount": 22,
            "checkRunCountsByState": [
                { "state": "CANCELLED", "count": 1 }, { "state": "FAILURE", "count": 10 },
                { "state": "IN_PROGRESS", "count": 1 }, { "state": "NEUTRAL", "count": 2 },
                { "state": "SKIPPED", "count": 1 }, { "state": "SUCCESS", "count": 7 },
            ],
            "statusContextCountsByState": [],
        });
        let counts = parse_detail(&detail).unwrap().check_counts.unwrap();
        let expected = CheckCounts {
            passed: 7,
            skipped: 3,
            pending: 1,
            failing: 11,
        };
        assert_eq!(counts, expected);
    }

    #[test]
    fn reviewers_join_requests_comments_and_teams() {
        let mut detail = detail_fixture();
        detail["reviewRequests"] = json!({ "nodes": [
            { "asCodeOwner": true, "requestedReviewer": { "name": "DATA" } },
        ]});
        detail["latestOpinionatedReviews"] = json!({ "nodes": [
            { "author": { "login": "peer" }, "state": "APPROVED" },
        ]});
        detail["latestReviews"] = json!({ "nodes": [
            { "author": { "login": "joey" }, "state": "COMMENTED",
              "onBehalfOf": { "nodes": [{ "name": "Core" }] } },
            { "author": { "login": "peer" }, "state": "COMMENTED", "onBehalfOf": { "nodes": [] } },
        ]});
        let reviewers = parse_detail(&detail).unwrap().reviewers;
        let rows: Vec<(&str, &str, bool, Option<&str>)> = reviewers
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.state.as_str(),
                    r.code_owner,
                    r.team.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("DATA", "requested", true, None),
                ("joey", "commented", false, Some("Core")),
                ("peer", "approved", false, None),
            ],
            "an approval outranks a later plain comment"
        );
    }

    #[test]
    fn since_counts_what_others_did_after_your_last_reply() {
        let mut activity = activity_fixture();
        // Your reply in the fixture is at 2026-11-14T11:00.
        activity["timelineItems"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!(
                { "__typename": "IssueComment", "author": { "__typename": "User", "login": "lead" },
                  "createdAt": "2026-11-15T10:00:00Z", "body": "Ping" }
            ));
        activity["timelineItems"]["nodes"].as_array_mut().unwrap().push(json!(
            { "__typename": "PullRequestReview", "id": "R3", "author": { "__typename": "User", "login": "lead" },
              "state": "COMMENTED", "submittedAt": "2026-11-15T11:00:00Z",
              "createdAt": "2026-11-15T11:00:00Z", "body": "" }
        ));
        activity["commitList"]["nodes"][1]["commit"]["committedDate"] =
            json!("2026-11-15T09:00:00Z");
        activity["events"] = json!({ "nodes": [
            { "__typename": "HeadRefForcePushedEvent", "createdAt": "2026-11-15T09:00:00Z" },
            { "__typename": "ReviewRequestedEvent", "createdAt": "2026-11-12T09:00:00Z" },
            { "__typename": "ReviewRequestedEvent", "createdAt": "2026-11-16T09:00:00Z" },
        ]});
        let parsed = parse_activity(&activity).unwrap();
        let since = parsed.since;
        assert_eq!(
            since.since.unwrap().to_rfc3339(),
            "2026-11-14T11:00:00+00:00"
        );
        assert_eq!(
            (
                since.commits,
                since.pushes,
                since.review_comments,
                since.comments,
                since.review_requests
            ),
            (1, 1, 1, 1, 1)
        );
        assert_eq!((parsed.threads_total, parsed.threads_unresolved), (2, 1));
    }

    #[test]
    fn a_skipped_check_neither_passes_nor_fails() {
        assert_eq!(
            check_run_state("COMPLETED", Some("SKIPPED")),
            CheckState::Skipped
        );
        assert_eq!(
            check_run_state("COMPLETED", Some("NEUTRAL")),
            CheckState::Passing
        );
        assert!(
            CheckState::Passing < CheckState::Skipped,
            "skipped checks list last"
        );
    }

    #[test]
    fn your_pending_review_is_drafts_not_conversation() {
        let mut activity = activity_fixture();
        let pending = json!({ "__typename": "PullRequestReview", "id": "P1",
            "author": { "__typename": "User", "login": "me" }, "viewerDidAuthor": true,
            "state": "PENDING", "submittedAt": null, "createdAt": "2026-11-15T09:00:00Z", "body": "" });
        activity["timelineItems"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(pending);
        let thread = json!({ "isResolved": false, "path": "c.md", "line": 9, "originalLine": 9,
            "startLine": 7, "originalStartLine": 7, "diffSide": "LEFT",
            "comments": { "nodes": [{ "id": "C1", "pullRequestReview": { "id": "P1", "state": "PENDING" },
                "author": { "__typename": "User", "login": "me" },
                "createdAt": "2026-11-15T09:00:00Z", "body": "Draft note" }] } });
        activity["reviewThreads"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(thread);
        // A reply you wrote with r while the review was pending, in a submitted thread.
        let reply = json!({ "id": "C2", "pullRequestReview": { "id": "P1", "state": "PENDING" },
            "author": { "__typename": "User", "login": "me" }, "viewerDidAuthor": true,
            "createdAt": "2026-11-15T09:30:00Z", "body": "Pending reply" });
        activity["reviewThreads"]["nodes"][1]["comments"]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(reply);
        activity["pendingReviews"] = json!({ "nodes": [{ "id": "P1" }] });
        let parsed = parse_activity(&activity).unwrap();
        assert_eq!(
            (parsed.threads_total, parsed.threads_unresolved),
            (2, 1),
            "drafts are not counted"
        );
        assert_eq!(parsed.pending_review.as_deref(), Some("P1"));
        assert_eq!(
            parsed.drafts,
            [
                Draft {
                    id: "C1".to_string(),
                    path: "c.md".to_string(),
                    side: Side::Left,
                    line: 9,
                    start_line: Some(7),
                    body: "Draft note".to_string(),
                    reply: false,
                },
                Draft {
                    id: "C2".to_string(),
                    path: "b.md".to_string(),
                    side: Side::Right,
                    line: 12,
                    start_line: None,
                    body: "Pending reply".to_string(),
                    reply: true,
                },
            ]
        );
        let replies: Vec<bool> = parsed
            .threads_on("b.md")
            .flat_map(|thread| &thread.comments)
            .map(|comment| comment.pending)
            .collect();
        assert_eq!(
            replies,
            [false, false, true],
            "the thread marks your pending reply"
        );
        assert!(
            parsed
                .timeline
                .iter()
                .all(|comment| comment.author.login != "me"),
            "no pending review in the conversation"
        );
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
            2,
            "the empty COMMENTED review is a thread reply"
        );
        let bot = &timeline[0];
        assert_eq!(
            bot.author,
            Author {
                login: "coderabbitai".to_string(),
                bot: true
            }
        );
        let review = &timeline[1];
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

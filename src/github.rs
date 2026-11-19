//! Talks to GitHub through `gh api graphql`, so the tool reuses the user's gh login.

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde_json::Value;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;

use crate::config::GroupConfig;
use crate::model::{
    self, ChangedFile, Group, HunkLine, MergeRequest, PrActivity, PrDetail, RateLimit, Side,
    TIMELINE_LIMIT, Verdict,
};

/// The GraphQL budgets the responses reported, one per reset time. GitHub reports two
/// counters with different reset times for one token, picking one per response, so
/// keeping only the latest made the count flip between them. Calls run on worker
/// threads; the UI reads this before each draw.
static RATE_LIMITS: Mutex<Vec<RateLimit>> = Mutex::new(Vec::new());

/// The budget closest to running out, or None before the first call.
pub fn rate_limit() -> Option<RateLimit> {
    let limits = RATE_LIMITS.lock().ok()?;
    limits.iter().min_by_key(|limit| limit.remaining).copied()
}

/// Adds a response's budget. Within one counter the count only falls, so a response that
/// arrives late cannot raise it; a counter goes once its reset time passes.
fn record_rate_limit(
    limits: &mut Vec<RateLimit>,
    latest: RateLimit,
    now: chrono::DateTime<chrono::Utc>,
) {
    limits.retain(|limit| limit.reset_at > now);
    match limits
        .iter_mut()
        .find(|limit| limit.reset_at == latest.reset_at)
    {
        Some(known) => known.remaining = known.remaining.min(latest.remaining),
        None => limits.push(latest),
    }
}

/// GitHub's maximum page size for a search connection.
const PAGE_SIZE: u32 = 100;

const ROW_FIELDS: &str = "number title url isDraft updatedAt reviewDecision totalCommentsCount \
    additions deletions changedFiles author { login } repository { nameWithOwner }";

const ROLLUP_COUNTS: &str = "totalCount \
    checkRunCountsByState { state count } \
    statusContextCountsByState { state count }";

pub fn search_query(group_query: &str, updated_since: Option<NaiveDate>) -> String {
    let mut query = format!("is:pr is:open archived:false {group_query}");
    if let Some(date) = updated_since {
        query.push_str(&format!(" updated:>={date}"));
    }
    // A group past one page keeps its newest PRs; a group's own sort wins.
    if !group_query.contains("sort:") {
        query.push_str(" sort:updated-desc");
    }
    query
}

/// One group's search. Each group is its own request, so the groups load in parallel and
/// a fast group shows before a slow one.
pub fn group_document() -> String {
    format!(
        "query($q: String!) {{ search(query: $q, type: ISSUE, first: {PAGE_SIZE}) {{ issueCount nodes {{ \
         ... on PullRequest {{ {ROW_FIELDS} \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 0) {{ {ROLLUP_COUNTS} }} }} }} }} }} }} }} }} }}"
    )
}

const PR_VARIABLES: &str = "$owner: String!, $name: String!, $number: Int!";

/// The header, description and sidebar: what the Overview shows first.
pub fn detail_document() -> String {
    format!(
        "query({PR_VARIABLES}) {{ \
         repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ \
         {ROW_FIELDS} id viewerDidAuthor baseRefOid headRefOid body createdAt baseRefName headRefName \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 0) {{ {ROLLUP_COUNTS} }} }} }} }} }} \
         commitCount: commits {{ totalCount }} \
         labels(first: 20) {{ nodes {{ name color }} }} \
         assignees(first: 10) {{ nodes {{ login }} }} \
         reviewRequests(first: 20) {{ nodes {{ asCodeOwner requestedReviewer {{ \
           ... on User {{ login }} ... on Bot {{ login }} ... on Mannequin {{ login }} ... on Team {{ name }} }} }} }} \
         latestOpinionatedReviews(first: 20) {{ nodes {{ author {{ login }} state }} }} \
         latestReviews(first: 30) {{ nodes {{ author {{ login }} state onBehalfOf(first: 5) {{ nodes {{ name }} }} }} }} \
         mergeStateStatus milestone {{ title }} state mergedAt closedAt mergedBy {{ login }} \
         closingIssuesReferences(first: 10) {{ nodes {{ number title url }} }} \
         viewerCanMergeAsAdmin viewerCanEnableAutoMerge viewerCanDisableAutoMerge \
         autoMergeRequest {{ mergeMethod }} \
         baseRepository {{ viewerPermission mergeCommitAllowed squashMergeAllowed \
           rebaseMergeAllowed viewerDefaultMergeMethod }} \
         }} }} }}"
    )
}

/// Checks, commits and the conversation, fetched at the same time as the detail.
pub fn activity_document() -> String {
    format!(
        "query({PR_VARIABLES}) {{ \
         repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 100) {{ {ROLLUP_COUNTS} \
           nodes {{ __typename \
             ... on CheckRun {{ name conclusion status detailsUrl startedAt completedAt \
               checkSuite {{ workflowRun {{ workflow {{ name }} }} }} }} \
             ... on StatusContext {{ context state targetUrl createdAt }} }} }} }} }} }} }} \
         commitList: commits(first: 100) {{ nodes {{ commit {{ \
           oid message authoredDate committedDate url author {{ name user {{ login }} }} \
           statusCheckRollup {{ state }} }} }} }} \
         timelineItems(last: {TIMELINE_LIMIT}, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW]) {{ nodes {{ __typename \
           ... on IssueComment {{ author {{ __typename login }} createdAt body viewerDidAuthor }} \
           ... on PullRequestReview {{ id viewerDidAuthor author {{ __typename login }} state submittedAt createdAt body }} }} }} \
         events: timelineItems(last: {TIMELINE_LIMIT}, itemTypes: [HEAD_REF_FORCE_PUSHED_EVENT, BASE_REF_CHANGED_EVENT, BASE_REF_FORCE_PUSHED_EVENT, REVIEW_REQUESTED_EVENT]) {{ nodes {{ __typename \
           ... on HeadRefForcePushedEvent {{ createdAt }} ... on BaseRefChangedEvent {{ createdAt }} \
           ... on BaseRefForcePushedEvent {{ createdAt }} ... on ReviewRequestedEvent {{ createdAt }} }} }} \
         reviewThreads(first: 100) {{ nodes {{ id isResolved path line originalLine startLine originalStartLine \
           diffSide comments(first: 50) {{ nodes {{ pullRequestReview {{ id state }} viewerDidAuthor author {{ __typename login }} createdAt body }} }} }} }} \
         pendingReviews: reviews(first: 1, states: PENDING) {{ nodes {{ id }} }} \
         }} }} }}"
    )
}

pub fn fetch_group(group: &GroupConfig, updated_since: Option<NaiveDate>) -> Result<Group> {
    let args = [
        "-f".to_string(),
        format!("query={}", group_document()),
        "-f".to_string(),
        format!("q={}", search_query(&group.query, updated_since)),
    ];
    let data = run_graphql(&args)?;
    let search = data.get("search").context("response has no search field")?;
    model::parse_group(search, group)
}

fn fetch_pull_request(document: &str, repo: &str, number: u64) -> Result<Value> {
    let (owner, name) = split_repo(repo)?;
    let args = [
        "-f".to_string(),
        format!("query={document}"),
        "-f".to_string(),
        format!("owner={owner}"),
        "-f".to_string(),
        format!("name={name}"),
        "-F".to_string(),
        format!("number={number}"),
    ];
    let data = run_graphql(&args)?;
    data.pointer("/repository/pullRequest")
        .filter(|value| !value.is_null())
        .cloned()
        .with_context(|| format!("{repo}#{number} not found"))
}

pub fn fetch_detail(repo: &str, number: u64) -> Result<PrDetail> {
    model::parse_detail(&fetch_pull_request(&detail_document(), repo, number)?)
}

pub fn fetch_activity(repo: &str, number: u64) -> Result<PrActivity> {
    model::parse_activity(&fetch_pull_request(&activity_document(), repo, number)?)
}

/// The diff hunk under each review thread. A hunk runs from its header down to the
/// commented line, so on a big pull request the hunks can outweigh the whole
/// conversation; they come in a call of their own, after the comments show.
fn hunks_document() -> String {
    format!(
        "query({PR_VARIABLES}) {{ repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ \
         reviewThreads(first: 100) {{ nodes {{ id comments(first: 1) {{ nodes {{ diffHunk }} }} }} }} }} }} }}"
    )
}

pub fn fetch_hunks(repo: &str, number: u64) -> Result<HashMap<String, Vec<HunkLine>>> {
    model::parse_hunks(&fetch_pull_request(&hunks_document(), repo, number)?)
}

const FILES_DOCUMENT: &str = "query($owner: String!, $name: String!, $number: Int!, $cursor: String) { \
    repository(owner: $owner, name: $name) { pullRequest(number: $number) { \
    files(first: 100, after: $cursor) { totalCount pageInfo { hasNextPage endCursor } \
    nodes { path additions deletions changeType viewerViewedState } } } } }";

/// Follows the cursor until GitHub reports no next page. After each page, `progress` gets
/// the files fetched so far and the PR's total.
pub fn fetch_files(
    repo: &str,
    number: u64,
    mut progress: impl FnMut(usize, usize),
) -> Result<Vec<ChangedFile>> {
    let (owner, name) = split_repo(repo)?;
    let mut files = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut args = vec![
            "-f".to_string(),
            format!("query={FILES_DOCUMENT}"),
            "-f".to_string(),
            format!("owner={owner}"),
            "-f".to_string(),
            format!("name={name}"),
            "-F".to_string(),
            format!("number={number}"),
        ];
        if let Some(after) = &cursor {
            args.extend(["-f".to_string(), format!("cursor={after}")]);
        }
        let data = run_graphql(&args)?;
        let raw = data
            .pointer("/repository/pullRequest/files")
            .with_context(|| format!("{repo}#{number} has no files field"))?;
        let page = model::parse_files_page(raw)?;
        files.extend(page.files);
        progress(files.len(), page.total);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => return Ok(files),
        }
    }
}

/// Sets the viewer's Viewed checkbox, the same one the web shows, for each path: one
/// request with one mutation per file.
pub fn set_viewed(pull_request_id: &str, paths: &[String], viewed: bool) -> Result<()> {
    let mut args = vec![
        "-f".to_string(),
        format!("query={}", viewed_document(paths.len(), viewed)),
        "-f".to_string(),
        format!("id={pull_request_id}"),
    ];
    for (index, path) in paths.iter().enumerate() {
        args.extend(["-f".to_string(), format!("p{index}={path}")]);
    }
    run_graphql(&args)?;
    Ok(())
}

fn viewed_document(count: usize, viewed: bool) -> String {
    let mutation = if viewed {
        "markFileAsViewed"
    } else {
        "unmarkFileAsViewed"
    };
    let variables: String = (0..count)
        .map(|index| format!(", $p{index}: String!"))
        .collect();
    let fields: String = (0..count)
        .map(|index| {
            format!(
                " f{index}: {mutation}(input: {{ pullRequestId: $id, path: $p{index} }}) {{ clientMutationId }}"
            )
        })
        .collect();
    format!("mutation($id: ID!{variables}) {{{fields} }}")
}

/// A line comment to add to your pending review.
pub struct NewComment<'a> {
    pub path: &'a str,
    pub side: Side,
    pub line: u64,
    pub start_line: Option<u64>,
    pub body: &'a str,
}

/// A label of the repo, with the node id the label mutations take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoLabel {
    pub id: String,
    pub name: String,
    pub color: String,
}

/// `-f` sends a string variable and `-F` a typed one, such as a number.
fn graphql_args(document: &str, strings: &[(&str, &str)], numbers: &[(&str, u64)]) -> Vec<String> {
    let mut args = vec!["-f".to_string(), format!("query={document}")];
    for (name, value) in strings {
        args.extend(["-f".to_string(), format!("{name}={value}")]);
    }
    for (name, value) in numbers {
        args.extend(["-F".to_string(), format!("{name}={value}")]);
    }
    args
}

/// The PR a review goes on, and the head commit the diff showed. A review revu creates
/// is pinned to that commit, so it covers what you saw, not commits pushed since.
pub struct Review<'a> {
    pub pull_request_id: &'a str,
    pub head_oid: &'a str,
}

/// Creates your pending review: a review with no verdict, which GitHub keeps hidden
/// from others until you submit it. GitHub allows one per person per PR.
fn start_pending_review(review: &Review) -> Result<String> {
    let document = "mutation($pr: ID!, $commit: GitObjectID!) { addPullRequestReview(input: { \
                    pullRequestId: $pr, commitOID: $commit }) { pullRequestReview { id } } }";
    let strings = [("pr", review.pull_request_id), ("commit", review.head_oid)];
    let response = run_graphql(&graphql_args(document, &strings, &[]))?;
    let id = response.pointer("/addPullRequestReview/pullRequestReview/id");
    id.and_then(Value::as_str)
        .map(str::to_string)
        .context("GitHub did not create the pending review")
}

/// Your pending review on the PR, asked fresh: the app's copy may predate a comment you
/// just added, or a review you started on the web.
fn find_pending_review(pull_request_id: &str) -> Result<Option<String>> {
    let document = "query($pr: ID!) { node(id: $pr) { ... on PullRequest { \
                    reviews(first: 1, states: PENDING) { nodes { id } } } } }";
    let response = run_graphql(&graphql_args(document, &[("pr", pull_request_id)], &[]))?;
    let id = response.pointer("/node/reviews/nodes/0/id");
    Ok(id.and_then(Value::as_str).map(str::to_string))
}

/// Adds a line comment to your pending review, creating the review first when there is
/// none. Returns the pending review's id.
pub fn add_line_comment(
    target: &Review,
    pending_review: Option<&str>,
    comment: &NewComment,
) -> Result<String> {
    let review = match pending_review {
        Some(review) => review.to_string(),
        None => match find_pending_review(target.pull_request_id)? {
            Some(review) => review,
            None => start_pending_review(target)?,
        },
    };
    let document = "mutation($review: ID!, $path: String!, $body: String!, $side: DiffSide!, \
                    $line: Int!, $startLine: Int) { addPullRequestReviewThread(input: { \
                    pullRequestReviewId: $review, path: $path, body: $body, side: $side, \
                    line: $line, startLine: $startLine, startSide: $side }) { thread { id } } }";
    let strings = [
        ("review", review.as_str()),
        ("path", comment.path),
        ("body", comment.body),
        ("side", comment.side.graphql()),
    ];
    let mut numbers = vec![("line", comment.line)];
    numbers.extend(comment.start_line.map(|start| ("startLine", start)));
    let response = run_graphql(&graphql_args(document, &strings, &numbers))?;
    // A line outside the diff gets a null thread and no error at all.
    if response
        .pointer("/addPullRequestReviewThread/thread")
        .is_none_or(Value::is_null)
    {
        bail!(
            "GitHub did not take the comment on {} line {}: only lines in the diff take comments",
            comment.path,
            comment.line
        );
    }
    Ok(review)
}

/// Submits your pending review with a verdict, or sends a new review in one step when
/// there is no pending one.
pub fn submit_review(
    target: &Review,
    pending_review: Option<&str>,
    verdict: Verdict,
    body: &str,
) -> Result<()> {
    let found = match pending_review {
        Some(review) => Some(review.to_string()),
        None => find_pending_review(target.pull_request_id)?,
    };
    let args = match found.as_deref() {
        Some(review) => graphql_args(
            "mutation($review: ID!, $event: PullRequestReviewEvent!, $body: String) { \
             submitPullRequestReview(input: { pullRequestReviewId: $review, event: $event, \
             body: $body }) { pullRequestReview { id } } }",
            &[
                ("review", review),
                ("event", verdict.graphql()),
                ("body", body),
            ],
            &[],
        ),
        None => graphql_args(
            "mutation($pr: ID!, $commit: GitObjectID!, $event: PullRequestReviewEvent!, \
             $body: String) { addPullRequestReview(input: { pullRequestId: $pr, \
             commitOID: $commit, event: $event, body: $body }) { pullRequestReview { id } } }",
            &[
                ("pr", target.pull_request_id),
                ("commit", target.head_oid),
                ("event", verdict.graphql()),
                ("body", body),
            ],
            &[],
        ),
    };
    run_graphql(&args)?;
    Ok(())
}

/// Replies to a review thread. With a pending review of yours on the PR, GitHub puts the
/// reply in it, as its web page does; otherwise the reply is posted at once.
pub fn reply_to_thread(thread: &str, body: &str) -> Result<()> {
    let document = "mutation($thread: ID!, $body: String!) { addPullRequestReviewThreadReply(\
                    input: { pullRequestReviewThreadId: $thread, body: $body }) { comment { id } } }";
    let response = run_graphql(&graphql_args(
        document,
        &[("thread", thread), ("body", body)],
        &[],
    ))?;
    if response
        .pointer("/addPullRequestReviewThreadReply/comment")
        .is_none_or(Value::is_null)
    {
        bail!("GitHub did not take the reply");
    }
    Ok(())
}

/// Resolves a review thread, or opens it again.
pub fn set_thread_resolved(thread: &str, resolved: bool) -> Result<()> {
    let mutation = if resolved {
        "resolveReviewThread"
    } else {
        "unresolveReviewThread"
    };
    let document = format!(
        "mutation($thread: ID!) {{ {mutation}(input: {{ threadId: $thread }}) {{ thread {{ isResolved }} }} }}"
    );
    let response = run_graphql(&graphql_args(&document, &[("thread", thread)], &[]))?;
    let now = response.pointer(&format!("/{mutation}/thread/isResolved"));
    if now.and_then(Value::as_bool) != Some(resolved) {
        bail!(
            "GitHub did not {} the thread",
            if resolved { "resolve" } else { "reopen" }
        );
    }
    Ok(())
}

/// Deletes your pending review and every line comment in it.
pub fn discard_review(pending_review: &str) -> Result<()> {
    let document = "mutation($review: ID!) { deletePullRequestReview(input: { \
                    pullRequestReviewId: $review }) { pullRequestReview { id } } }";
    run_graphql(&graphql_args(document, &[("review", pending_review)], &[]))?;
    Ok(())
}

/// Merges the PR, or turns auto-merge on or off. `head_oid` is the head you saw: GitHub
/// refuses the merge when someone pushed after it, so revu never merges unseen commits.
pub fn merge(pull_request_id: &str, head_oid: &str, request: MergeRequest) -> Result<()> {
    let (mutation, method) = match request {
        MergeRequest::Now(method) => ("mergePullRequest", method),
        MergeRequest::Auto(method) => ("enablePullRequestAutoMerge", method),
        MergeRequest::DisableAuto => {
            let document = "mutation($pr: ID!) { disablePullRequestAutoMerge(input: { \
                            pullRequestId: $pr }) { clientMutationId } }";
            run_graphql(&graphql_args(document, &[("pr", pull_request_id)], &[]))?;
            return Ok(());
        }
    };
    let document = format!(
        "mutation($pr: ID!, $head: GitObjectID!, $method: PullRequestMergeMethod!) {{ \
         {mutation}(input: {{ pullRequestId: $pr, expectedHeadOid: $head, \
         mergeMethod: $method }}) {{ clientMutationId }} }}"
    );
    let strings = [
        ("pr", pull_request_id),
        ("head", head_oid),
        ("method", method.graphql()),
    ];
    run_graphql(&graphql_args(&document, &strings, &[]))?;
    Ok(())
}

/// Every label of the repo, by name. One page holds 100; more are rare.
pub fn repo_labels(repo: &str) -> Result<Vec<RepoLabel>> {
    let (owner, name) = repo.split_once('/').context("repo is not owner/name")?;
    let document = "query($owner: String!, $name: String!) { repository(owner: $owner, \
                    name: $name) { labels(first: 100, orderBy: { field: NAME, direction: ASC }) \
                    { nodes { id name color } } } }";
    let response = run_graphql(&graphql_args(
        document,
        &[("owner", owner), ("name", name)],
        &[],
    ))?;
    let nodes = response
        .pointer("/repository/labels/nodes")
        .and_then(Value::as_array)
        .context("GitHub returned no labels")?;
    let label = |node: &Value| {
        let field = |key: &str| node.get(key).and_then(Value::as_str).map(str::to_string);
        Some(RepoLabel {
            id: field("id")?,
            name: field("name")?,
            color: field("color").unwrap_or_default(),
        })
    };
    Ok(nodes.iter().filter_map(label).collect())
}

/// Adds and removes labels on a PR, one call each.
/// Adds and removes labels in one request.
pub fn set_labels(pull_request_id: &str, add: &[String], remove: &[String]) -> Result<()> {
    let Some(document) = labels_document(!add.is_empty(), !remove.is_empty()) else {
        return Ok(());
    };
    let mut args = graphql_args(&document, &[("id", pull_request_id)], &[]);
    for (variable, ids) in [("add", add), ("remove", remove)] {
        for label in ids {
            args.extend(["-f".to_string(), format!("{variable}[]={label}")]);
        }
    }
    run_graphql(&args)?;
    Ok(())
}

/// The label mutations for one request: an add, a remove, or both. GraphQL refuses a
/// declared variable that goes unused, so each list is declared only when it is sent.
fn labels_document(add: bool, remove: bool) -> Option<String> {
    let parts = [
        (add, "add", "addLabelsToLabelable"),
        (remove, "remove", "removeLabelsFromLabelable"),
    ];
    let mut variables = String::from("$id: ID!");
    let mut fields = String::new();
    for (_, variable, mutation) in parts.iter().filter(|(sent, ..)| *sent) {
        variables.push_str(&format!(", ${variable}: [ID!]!"));
        fields.push_str(&format!(
            " {variable}: {mutation}(input: {{ labelableId: $id, labelIds: ${variable} }}) \
             {{ clientMutationId }}"
        ));
    }
    (add || remove).then(|| format!("mutation({variables}) {{{fields} }}"))
}

/// Splits `gh api -i` output into the header block and the body. Output without a status
/// line is all body.
fn split_headers(response: &str) -> (&str, &str) {
    if !response.starts_with("HTTP/") {
        return ("", response);
    }
    response
        .split_once("\r\n\r\n")
        .or_else(|| response.split_once("\n\n"))
        .unwrap_or((response, ""))
}

fn parse_rate_limit(headers: &str) -> Option<RateLimit> {
    let value = |name: &str| {
        headers.lines().find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().parse::<u64>().ok())?
        })
    };
    let reset = i64::try_from(value("x-ratelimit-reset")?).ok()?;
    Some(RateLimit {
        remaining: value("x-ratelimit-remaining")?,
        limit: value("x-ratelimit-limit")?,
        reset_at: chrono::DateTime::from_timestamp(reset, 0)?,
    })
}

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .with_context(|| format!("repo \"{repo}\" is not owner/name"))
}

/// How to install gh here, for the gh screen.
pub const GH_INSTALL: &str = if cfg!(target_os = "macos") {
    "brew install gh"
} else {
    "see https://cli.github.com for your system's package"
};

/// Why gh cannot reach GitHub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhProblem {
    /// gh is not on PATH.
    Missing,
    /// gh runs but has no working login; holds what gh said.
    LoggedOut(String),
    /// gh cannot reach GitHub; holds the network error gh hit.
    Offline(String),
}

/// What gh prints when the network, not the login, failed. gh then also calls the
/// token invalid, so these words decide which one it is.
const NETWORK_ERRORS: &[&str] = &[
    "dial tcp",
    "no such host",
    "i/o timeout",
    "network is unreachable",
    "connection refused",
    "connection reset",
    "TLS handshake timeout",
    "error connecting to",
];

/// The line of gh's output that names a network failure, without gh's prefixes.
pub fn network_error(said: &str) -> Option<String> {
    let line = said
        .lines()
        .rev()
        .find(|line| NETWORK_ERRORS.iter().any(|marker| line.contains(marker)))?;
    let line = line
        .trim()
        .trim_start_matches("- ")
        .trim_start_matches("Error: ");
    Some(line.to_string())
}

/// GitHub's rate limits as what to do, in place of gh's text and the raw JSON.
/// `limit` comes from the same response, so its reset time is the one that applies.
fn rate_limit_message(said: &str, limit: Option<&RateLimit>) -> Option<String> {
    let said = said.to_lowercase();
    if said.contains("secondary rate limit") {
        return Some(
            "GitHub's secondary rate limit was hit: too many requests in a short time. Wait a few minutes, then try again."
                .to_string(),
        );
    }
    if !said.contains("rate limit") {
        return None;
    }
    let reset = limit.map_or(String::new(), |limit| {
        let at = limit.reset_at.with_timezone(&chrono::Local);
        format!(" It resets at {}.", at.format("%H:%M"))
    });
    Some(format!("GitHub's API limit is used up.{reset}"))
}

/// Reads what `gh auth status` said when it failed.
fn classify_auth_failure(said: &str) -> GhProblem {
    match network_error(said) {
        Some(error) => GhProblem::Offline(error),
        None => GhProblem::LoggedOut(said.to_string()),
    }
}

/// Asks gh whether it is installed and logged in. It runs only after a fetch fails, so
/// a working setup pays nothing for it.
pub fn gh_problem() -> Option<GhProblem> {
    let output = match Command::new("gh").args(["auth", "status"]).output() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Some(GhProblem::Missing);
        }
        // gh could not start for another reason; the fetch error already says so.
        Err(_) => return None,
        Ok(output) => output,
    };
    if output.status.success() {
        return None;
    }
    let said = [output.stderr, output.stdout]
        .iter()
        .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
        .find(|text| !text.is_empty())
        .unwrap_or_default();
    Some(classify_auth_failure(&said))
}

/// Returns the GraphQL `data` object. A GraphQL error fails the call even when gh exits 0.
fn run_graphql(args: &[String]) -> Result<Value> {
    // -i adds the response headers, which carry the rate limit.
    let output = Command::new("gh")
        .args(["api", "graphql", "-i"])
        .args(args)
        .output()
        .context("run gh; is the GitHub CLI installed and on PATH?")?;
    let response = String::from_utf8_lossy(&output.stdout);
    let (headers, stdout) = split_headers(&response);
    let limit = parse_rate_limit(headers);
    if let Some(limit) = limit
        && let Ok(mut limits) = RATE_LIMITS.lock()
    {
        record_rate_limit(&mut limits, limit, chrono::Utc::now());
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let said = format!("{stderr}\n{stdout}");
        if let Some(message) = rate_limit_message(&said, limit.as_ref()) {
            bail!("{message}");
        }
        if let Some(error) = network_error(&stderr) {
            bail!("{error}");
        }
        bail!(
            "gh api graphql failed ({}): {} {}",
            output.status,
            stderr.trim(),
            stdout.trim()
        );
    }
    let response: Value = serde_json::from_str(stdout).context("gh returned invalid JSON")?;
    if let Some(errors) = response.get("errors") {
        if let Some(message) = rate_limit_message(&errors.to_string(), limit.as_ref()) {
            bail!("{message}");
        }
        bail!("GitHub GraphQL errors: {errors}");
    }
    response
        .get("data")
        .cloned()
        .context("GraphQL response has no data field")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_go_in_one_request_declaring_only_the_lists_sent() {
        assert_eq!(labels_document(false, false), None);
        let both = labels_document(true, true).unwrap();
        assert!(
            both.starts_with("mutation($id: ID!, $add: [ID!]!, $remove: [ID!]!)"),
            "{both}"
        );
        assert!(
            both.contains("add: addLabelsToLabelable")
                && both.contains("remove: removeLabelsFromLabelable")
        );
        let remove_only = labels_document(false, true).unwrap();
        assert!(
            !remove_only.contains("$add") && !remove_only.contains("addLabels"),
            "{remove_only}"
        );
    }

    #[test]
    fn rate_limits_read_as_what_to_do() {
        let limit = RateLimit {
            remaining: 0,
            limit: 5000,
            reset_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
        };
        let at = limit.reset_at.with_timezone(&chrono::Local).format("%H:%M");
        assert_eq!(
            rate_limit_message(
                "gh: API rate limit already exceeded for user ID 1.",
                Some(&limit)
            ),
            Some(format!("GitHub's API limit is used up. It resets at {at}."))
        );
        assert!(
            rate_limit_message("You have exceeded a secondary rate limit.", Some(&limit))
                .unwrap()
                .contains("Wait a few minutes")
        );
        assert_eq!(
            rate_limit_message("Could not resolve to a PullRequest", None),
            None
        );
    }

    #[test]
    fn a_fetch_error_names_its_network_failure() {
        assert_eq!(
            network_error("Post \"https://api.github.com/graphql\": proxyconnect tcp: dial tcp 10.0.0.1:9: i/o timeout").as_deref(),
            Some("Post \"https://api.github.com/graphql\": proxyconnect tcp: dial tcp 10.0.0.1:9: i/o timeout")
        );
        assert_eq!(network_error("HTTP 502: Bad Gateway"), None);
    }

    #[test]
    fn a_network_failure_is_not_read_as_a_bad_login() {
        let offline = "github.com\n  X Failed to log in to github.com account someone (keyring)\n  - The token in keyring is invalid.\n  - Error: Get \"https://api.github.com/\": proxyconnect tcp: dial tcp 127.0.0.1:9: connect: connection refused";
        assert_eq!(
            classify_auth_failure(offline),
            GhProblem::Offline(
                "Get \"https://api.github.com/\": proxyconnect tcp: dial tcp 127.0.0.1:9: connect: connection refused".to_string()
            )
        );
        let logged_out = "You are not logged into any GitHub hosts. To log in, run: gh auth login";
        assert_eq!(
            classify_auth_failure(logged_out),
            GhProblem::LoggedOut(logged_out.to_string())
        );
    }

    #[test]
    fn the_budget_shown_never_flips_between_githubs_two_counters() {
        let at = |minutes: i64| {
            chrono::DateTime::from_timestamp(1_800_000_000 + minutes * 60, 0).unwrap()
        };
        let limit = |remaining: u64, reset: i64| RateLimit {
            remaining,
            limit: 5000,
            reset_at: at(reset),
        };
        let mut limits = Vec::new();
        record_rate_limit(&mut limits, limit(3838, 30), at(0));
        record_rate_limit(&mut limits, limit(4992, 60), at(0));
        record_rate_limit(&mut limits, limit(3840, 30), at(1));
        let lowest = limits.iter().min_by_key(|limit| limit.remaining).unwrap();
        assert_eq!(
            lowest.remaining, 3838,
            "the other counter and a late response do not raise it"
        );
        record_rate_limit(&mut limits, limit(4990, 60), at(31));
        assert_eq!(limits, [limit(4990, 60)], "a counter goes once it resets");
    }

    #[test]
    fn the_rate_limit_comes_from_the_response_headers() {
        let response = "HTTP/2.0 200 OK\nX-Ratelimit-Limit: 5000\nX-Ratelimit-Remaining: 4073\nX-Ratelimit-Reset: 1794914863\n\n{\"data\":{}}";
        let (headers, body) = split_headers(response);
        assert_eq!(body, "{\"data\":{}}");
        let limit = parse_rate_limit(headers).unwrap();
        assert_eq!((limit.remaining, limit.limit), (4073, 5000));
        assert_eq!(limit.reset_at.timestamp(), 1794914863);
        assert_eq!(split_headers("{}"), ("", "{}"), "a body alone stays whole");
    }

    #[test]
    fn search_query_scopes_to_open_prs_and_adds_the_date() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 18).unwrap();
        assert_eq!(
            search_query("author:@me", Some(date)),
            "is:pr is:open archived:false author:@me updated:>=2026-10-18 sort:updated-desc"
        );
        assert_eq!(
            search_query("author:@me sort:created-asc", None),
            "is:pr is:open archived:false author:@me sort:created-asc"
        );
    }

    #[test]
    fn group_document_takes_one_search_variable() {
        let document = group_document();
        assert!(document.starts_with("query($q: String!)"));
        assert!(document.contains("search(query: $q, type: ISSUE, first: 100)"));
    }
}

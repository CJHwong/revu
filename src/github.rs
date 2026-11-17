//! Talks to GitHub through `gh api graphql`, so the tool reuses the user's gh login.

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde_json::Value;
use std::process::Command;
use std::sync::Mutex;

use crate::config::GroupConfig;
use crate::model::{
    self, ChangedFile, Group, PrActivity, PrDetail, RateLimit, Side, TIMELINE_LIMIT, Verdict,
};

/// The budget from the latest GraphQL response. Calls run on worker threads; the UI reads
/// this before each draw.
static RATE_LIMIT: Mutex<Option<RateLimit>> = Mutex::new(None);

/// The budget after the latest call, or None before the first one.
pub fn rate_limit() -> Option<RateLimit> {
    RATE_LIMIT.lock().map_or(None, |limit| *limit)
}

/// GitHub's maximum page size for a search connection.
const PAGE_SIZE: u32 = 100;

const ROW_FIELDS: &str = "number title url isDraft updatedAt reviewDecision totalCommentsCount \
    author { login } repository { nameWithOwner }";

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
         {ROW_FIELDS} id viewerDidAuthor baseRefOid headRefOid body createdAt additions deletions changedFiles baseRefName headRefName \
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
         reviewThreads(first: 100) {{ nodes {{ isResolved path line originalLine startLine originalStartLine \
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

/// Sets the viewer's Viewed checkbox for one file, the same one the web shows.
pub fn set_viewed(pull_request_id: &str, path: &str, viewed: bool) -> Result<()> {
    let mutation = if viewed {
        "markFileAsViewed"
    } else {
        "unmarkFileAsViewed"
    };
    let document = format!(
        "mutation($id: ID!, $path: String!) {{ {mutation}(input: {{ pullRequestId: $id, path: $path }}) {{ clientMutationId }} }}"
    );
    let args = [
        "-f".to_string(),
        format!("query={document}"),
        "-f".to_string(),
        format!("id={pull_request_id}"),
        "-f".to_string(),
        format!("path={path}"),
    ];
    run_graphql(&args)?;
    Ok(())
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

/// Creates your pending review: a review with no verdict, which GitHub keeps hidden
/// from others until you submit it. GitHub allows one per person per PR.
fn start_pending_review(pull_request_id: &str) -> Result<String> {
    let document = "mutation($pr: ID!) { addPullRequestReview(input: { pullRequestId: $pr }) \
                    { pullRequestReview { id } } }";
    let response = run_graphql(&graphql_args(document, &[("pr", pull_request_id)], &[]))?;
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
    pull_request_id: &str,
    pending_review: Option<&str>,
    comment: &NewComment,
) -> Result<String> {
    let review = match pending_review {
        Some(review) => review.to_string(),
        None => match find_pending_review(pull_request_id)? {
            Some(review) => review,
            None => start_pending_review(pull_request_id)?,
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
    pull_request_id: &str,
    pending_review: Option<&str>,
    verdict: Verdict,
    body: &str,
) -> Result<()> {
    let found = match pending_review {
        Some(review) => Some(review.to_string()),
        None => find_pending_review(pull_request_id)?,
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
            "mutation($pr: ID!, $event: PullRequestReviewEvent!, $body: String) { \
             addPullRequestReview(input: { pullRequestId: $pr, event: $event, body: $body }) \
             { pullRequestReview { id } } }",
            &[
                ("pr", pull_request_id),
                ("event", verdict.graphql()),
                ("body", body),
            ],
            &[],
        ),
    };
    run_graphql(&args)?;
    Ok(())
}

/// Deletes your pending review and every line comment in it.
pub fn discard_review(pending_review: &str) -> Result<()> {
    let document = "mutation($review: ID!) { deletePullRequestReview(input: { \
                    pullRequestReviewId: $review }) { pullRequestReview { id } } }";
    run_graphql(&graphql_args(document, &[("review", pending_review)], &[]))?;
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
pub fn set_labels(pull_request_id: &str, add: &[String], remove: &[String]) -> Result<()> {
    for (mutation, ids) in [
        ("addLabelsToLabelable", add),
        ("removeLabelsFromLabelable", remove),
    ] {
        if ids.is_empty() {
            continue;
        }
        let document = format!(
            "mutation($id: ID!, $labels: [ID!]!) {{ {mutation}(input: {{ labelableId: $id, \
             labelIds: $labels }}) {{ clientMutationId }} }}"
        );
        let mut args = graphql_args(&document, &[("id", pull_request_id)], &[]);
        for label in ids {
            args.extend(["-f".to_string(), format!("labels[]={label}")]);
        }
        run_graphql(&args)?;
    }
    Ok(())
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
    if let Some(limit) = parse_rate_limit(headers)
        && let Ok(mut latest) = RATE_LIMIT.lock()
    {
        *latest = Some(limit);
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "gh api graphql failed ({}): {} {}",
            output.status,
            stderr.trim(),
            stdout.trim()
        );
    }
    let response: Value = serde_json::from_str(stdout).context("gh returned invalid JSON")?;
    if let Some(errors) = response.get("errors") {
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

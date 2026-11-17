//! Talks to GitHub through `gh api graphql`, so the tool reuses the user's gh login.

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde_json::Value;
use std::process::Command;

use crate::config::GroupConfig;
use crate::model::{self, ChangedFile, Group, PrActivity, PrDetail, TIMELINE_LIMIT};

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
         {ROW_FIELDS} id baseRefOid headRefOid body createdAt additions deletions changedFiles baseRefName headRefName \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 0) {{ {ROLLUP_COUNTS} }} }} }} }} }} \
         commitCount: commits {{ totalCount }} \
         labels(first: 20) {{ nodes {{ name color }} }} \
         assignees(first: 10) {{ nodes {{ login }} }} \
         reviewRequests(first: 20) {{ nodes {{ requestedReviewer {{ \
           ... on User {{ login }} ... on Bot {{ login }} ... on Mannequin {{ login }} ... on Team {{ name }} }} }} }} \
         latestOpinionatedReviews(first: 20) {{ nodes {{ author {{ login }} state }} }} \
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
           oid message authoredDate url author {{ name user {{ login }} }} \
           statusCheckRollup {{ state }} }} }} }} \
         timelineItems(last: {TIMELINE_LIMIT}, itemTypes: [ISSUE_COMMENT, PULL_REQUEST_REVIEW, PULL_REQUEST_COMMIT]) {{ nodes {{ __typename \
           ... on IssueComment {{ author {{ __typename login }} createdAt body viewerDidAuthor }} \
           ... on PullRequestReview {{ id viewerDidAuthor author {{ __typename login }} state submittedAt createdAt body }} \
           ... on PullRequestCommit {{ commit {{ oid messageHeadline authoredDate }} }} }} }} \
         reviewThreads(first: 100) {{ nodes {{ isResolved path line originalLine startLine originalStartLine \
           comments(first: 50) {{ nodes {{ pullRequestReview {{ id }} viewerDidAuthor author {{ __typename login }} createdAt body }} }} }} }} \
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
    files(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } \
    nodes { path additions deletions changeType viewerViewedState } } } } }";

/// Follows the cursor until GitHub reports no next page.
pub fn fetch_files(repo: &str, number: u64) -> Result<Vec<ChangedFile>> {
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

fn split_repo(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .with_context(|| format!("repo \"{repo}\" is not owner/name"))
}

/// Returns the GraphQL `data` object. A GraphQL error fails the call even when gh exits 0.
fn run_graphql(args: &[String]) -> Result<Value> {
    let output = Command::new("gh")
        .args(["api", "graphql"])
        .args(args)
        .output()
        .context("run gh; is the GitHub CLI installed and on PATH?")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "gh api graphql failed ({}): {} {}",
            output.status,
            stderr.trim(),
            stdout.trim()
        );
    }
    let response: Value = serde_json::from_str(&stdout).context("gh returned invalid JSON")?;
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

//! Talks to GitHub through `gh api graphql`, so the tool reuses the user's gh login.

use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde_json::Value;
use std::process::Command;

use crate::config::Config;
use crate::model::{self, ChangedFile, Group, PrDetail};

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
    query
}

/// One request for every group: each group is a `search` under the alias `g<index>`.
pub fn inbox_document(group_count: usize) -> String {
    let variables: Vec<String> = (0..group_count)
        .map(|index| format!("$q{index}: String!"))
        .collect();
    let searches: Vec<String> = (0..group_count)
        .map(|index| {
            format!("g{index}: search(query: $q{index}, type: ISSUE, first: {PAGE_SIZE}) {{ issueCount nodes {{ ...Row }} }}")
        })
        .collect();
    format!(
        "query({}) {{ {} }} \
         fragment Row on PullRequest {{ {ROW_FIELDS} \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 0) {{ {ROLLUP_COUNTS} }} }} }} }} }} }}",
        variables.join(", "),
        searches.join(" ")
    )
}

pub fn detail_document() -> String {
    format!(
        "query($owner: String!, $name: String!, $number: Int!) {{ \
         repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{ \
         {ROW_FIELDS} id baseRefOid headRefOid body createdAt additions deletions changedFiles baseRefName headRefName \
         commits(last: 1) {{ nodes {{ commit {{ statusCheckRollup {{ state contexts(first: 100) {{ {ROLLUP_COUNTS} \
           nodes {{ __typename \
             ... on CheckRun {{ name conclusion status detailsUrl startedAt completedAt \
               checkSuite {{ workflowRun {{ workflow {{ name }} }} }} }} \
             ... on StatusContext {{ context state targetUrl createdAt }} }} }} }} }} }} }} \
         commitList: commits(first: 100) {{ totalCount nodes {{ commit {{ \
           oid messageHeadline authoredDate url author {{ name user {{ login }} }} \
           statusCheckRollup {{ state }} }} }} }} \
         labels(first: 20) {{ nodes {{ name }} }} \
         assignees(first: 10) {{ nodes {{ login }} }} \
         reviewRequests(first: 20) {{ nodes {{ requestedReviewer {{ \
           ... on User {{ login }} ... on Bot {{ login }} ... on Mannequin {{ login }} ... on Team {{ name }} }} }} }} \
         latestOpinionatedReviews(first: 20) {{ nodes {{ author {{ login }} state }} }} \
         comments(last: 15) {{ nodes {{ author {{ login }} createdAt body }} }} \
         reviews(last: 15) {{ nodes {{ author {{ login }} createdAt state body }} }} \
         }} }} }}"
    )
}

pub fn fetch_inbox(config: &Config, today: NaiveDate) -> Result<Vec<Group>> {
    if config.groups.is_empty() {
        return Ok(Vec::new());
    }
    let updated_since = config
        .updated_within_days
        .map(|days| today - chrono::Duration::days(i64::from(days)));
    let mut args = vec![
        "-f".to_string(),
        format!("query={}", inbox_document(config.groups.len())),
    ];
    for (index, group) in config.groups.iter().enumerate() {
        args.push("-f".to_string());
        args.push(format!(
            "q{index}={}",
            search_query(&group.query, updated_since)
        ));
    }
    let data = run_graphql(&args)?;
    model::parse_inbox(&data, &config.groups)
}

pub fn fetch_detail(repo: &str, number: u64) -> Result<PrDetail> {
    let (owner, name) = split_repo(repo)?;
    let args = [
        "-f".to_string(),
        format!("query={}", detail_document()),
        "-f".to_string(),
        format!("owner={owner}"),
        "-f".to_string(),
        format!("name={name}"),
        "-F".to_string(),
        format!("number={number}"),
    ];
    let data = run_graphql(&args)?;
    let pull_request = data
        .pointer("/repository/pullRequest")
        .filter(|value| !value.is_null())
        .with_context(|| format!("{repo}#{number} not found"))?;
    model::parse_detail(pull_request)
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
            "is:pr is:open archived:false author:@me updated:>=2026-10-18"
        );
        assert_eq!(
            search_query("author:@me", None),
            "is:pr is:open archived:false author:@me"
        );
    }

    #[test]
    fn inbox_document_declares_one_variable_and_alias_per_group() {
        let document = inbox_document(2);
        assert!(document.starts_with("query($q0: String!, $q1: String!)"));
        assert!(document.contains("g0: search(query: $q0, type: ISSUE, first: 100)"));
        assert!(document.contains("g1: search(query: $q1, type: ISSUE, first: 100)"));
        assert!(!document.contains("$q2"));
    }
}

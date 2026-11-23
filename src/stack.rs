//! Stacked PRs: a PR whose base branch is another open PR's head sits on top of it.
//! The links come from branch names only, inside one repo.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{self, PrSummary, ReviewState};

/// One open PR, with only what linking and the Stack card need.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub number: u64,
    pub title: String,
    pub base: String,
    pub head: String,
    /// The head lives in a fork. Its branch name says nothing about this repo's
    /// branches, so no PR here can sit on top of it.
    pub cross_repo: bool,
    pub is_draft: bool,
    pub review: ReviewState,
}

impl Link {
    pub fn from_summary(pr: &PrSummary) -> Link {
        Link {
            number: pr.number,
            title: pr.title.clone(),
            base: pr.base.clone(),
            head: pr.head.clone(),
            cross_repo: pr.cross_repo,
            is_draft: pr.is_draft,
            review: pr.review,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLinks {
    page_info: RawPageInfo,
    nodes: Vec<RawLink>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLink {
    number: u64,
    title: String,
    base_ref_name: String,
    head_ref_name: String,
    is_cross_repository: bool,
    is_draft: bool,
    review_decision: Option<String>,
}

/// One page of a repo's open PRs, and the cursor of the next page when there is one.
pub fn parse_links_page(pull_requests: &Value) -> Result<(Vec<Link>, Option<String>)> {
    let raw: RawLinks =
        serde_json::from_value(pull_requests.clone()).context("parse the open PRs")?;
    let next = raw
        .page_info
        .end_cursor
        .filter(|_| raw.page_info.has_next_page);
    let links = raw
        .nodes
        .into_iter()
        .map(|node| Link {
            number: node.number,
            title: node.title,
            base: node.base_ref_name,
            head: node.head_ref_name,
            cross_repo: node.is_cross_repository,
            is_draft: node.is_draft,
            review: model::review_state(node.review_decision.as_deref()),
        })
        .collect();
    Ok((links, next))
}

/// A chain of two or more PRs in one repo, each based on the one before.
#[derive(Debug, Clone, PartialEq)]
pub struct Stack {
    /// The branch the bottom PR merges into.
    pub root: String,
    /// Bottom first.
    pub members: Vec<Link>,
}

impl Stack {
    /// The PR's place in the stack, counted from 1 at the bottom.
    pub fn position(&self, number: u64) -> Option<usize> {
        self.members
            .iter()
            .position(|member| member.number == number)
            .map(|index| index + 1)
    }

    pub fn bottom(&self) -> u64 {
        self.members[0].number
    }
}

/// The chains among one repo's open PRs. A PR with two PRs on top of it ends its
/// chain, and each of the two starts a chain of its own. A chain of one PR is not
/// a stack, so it is left out.
pub fn chains(links: &[Link]) -> Vec<Stack> {
    let by_head: HashMap<&str, &Link> = links
        .iter()
        .filter(|link| !link.cross_repo)
        .map(|link| (link.head.as_str(), link))
        .collect();
    let mut on_top: HashMap<&str, Vec<&Link>> = HashMap::new();
    for link in links {
        on_top.entry(link.base.as_str()).or_default().push(link);
    }
    let lone_child = |link: &Link| -> Option<&Link> {
        if link.cross_repo {
            return None;
        }
        match on_top.get(link.head.as_str()).map(Vec::as_slice) {
            Some([child]) => Some(child),
            _ => None,
        }
    };
    let starts_chain = |link: &Link| match by_head.get(link.base.as_str()) {
        None => true,
        Some(parent) => lone_child(parent).is_none(),
    };
    let mut stacks: Vec<Stack> = links
        .iter()
        .filter(|link| starts_chain(link))
        .map(|bottom| {
            let mut members = vec![bottom.clone()];
            let mut current = bottom;
            while let Some(child) = lone_child(current) {
                members.push(child.clone());
                current = child;
            }
            Stack {
                root: bottom.base.clone(),
                members,
            }
        })
        .filter(|stack| stack.members.len() > 1)
        .collect();
    stacks.sort_by_key(Stack::bottom);
    stacks
}

/// The stacks of several repos, and where each PR sits in them.
#[derive(Debug, Default)]
pub struct StackIndex {
    pub stacks: Vec<RepoStack>,
    places: HashMap<(String, u64), Place>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RepoStack {
    pub repo: String,
    pub stack: Stack,
    /// The links came from every open PR of the repo, not only the rows revu loaded,
    /// so no PR is missing from the chain.
    pub complete: bool,
}

impl RepoStack {
    /// The stack's identity across refreshes, for its fold state.
    pub fn key(&self) -> (String, u64) {
        (self.repo.clone(), self.stack.bottom())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// An index into `StackIndex::stacks`.
    pub stack: usize,
    /// Counted from 1 at the bottom.
    pub position: usize,
}

impl StackIndex {
    /// `complete` says whether `links` holds every open PR of the repo.
    pub fn add_repo(&mut self, repo: &str, links: &[Link], complete: bool) {
        for stack in chains(links) {
            let index = self.stacks.len();
            for (offset, member) in stack.members.iter().enumerate() {
                let place = Place {
                    stack: index,
                    position: offset + 1,
                };
                self.places.insert((repo.to_string(), member.number), place);
            }
            self.stacks.push(RepoStack {
                repo: repo.to_string(),
                stack,
                complete,
            });
        }
    }

    pub fn place(&self, repo: &str, number: u64) -> Option<Place> {
        self.places.get(&(repo.to_string(), number)).copied()
    }
}

/// What the Stack card lists around the PR at `position`: a run of PRs, or a gap of
/// PRs it leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// An index into the stack's members.
    Member(usize),
    /// How many members the card leaves out here.
    Gap(usize),
}

/// How many PRs the card keeps on each side of the open one.
const NEIGHBORS: usize = 2;

/// The bottom, the top, and the open PR with its neighbors; the rest folds into gaps.
/// `position` counts from 1. With `full`, every member shows.
pub fn window(length: usize, position: usize, full: bool) -> Vec<Slot> {
    let current = position.saturating_sub(1);
    let keep = |index: usize| {
        full || index == 0 || index + 1 == length || index.abs_diff(current) <= NEIGHBORS
    };
    let mut slots = Vec::new();
    let mut gap = 0;
    for index in 0..length {
        if keep(index) {
            if gap > 0 {
                slots.push(Slot::Gap(gap));
                gap = 0;
            }
            slots.push(Slot::Member(index));
        } else {
            gap += 1;
        }
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(number: u64, base: &str, head: &str) -> Link {
        Link {
            number,
            title: format!("PR {number}"),
            base: base.to_string(),
            head: head.to_string(),
            cross_repo: false,
            is_draft: false,
            review: ReviewState::Awaiting,
        }
    }

    fn numbers(stack: &Stack) -> Vec<u64> {
        stack.members.iter().map(|member| member.number).collect()
    }

    #[test]
    fn a_linear_chain_runs_bottom_to_top_in_any_input_order() {
        let links = vec![
            link(3, "b", "c"),
            link(1, "feature/x", "a"),
            link(2, "a", "b"),
            link(9, "main", "lone"),
        ];
        let stacks = chains(&links);
        assert_eq!(stacks.len(), 1, "a lone PR is not a stack");
        assert_eq!(numbers(&stacks[0]), vec![1, 2, 3]);
        assert_eq!(stacks[0].root, "feature/x");
        assert_eq!(stacks[0].position(3), Some(3));
        assert_eq!(stacks[0].position(9), None);
    }

    #[test]
    fn a_fork_head_never_becomes_a_parent() {
        // A fork's PR from its own `main` must not adopt every PR based on `main`.
        let mut fork = link(5, "main", "main");
        fork.cross_repo = true;
        let links = vec![fork, link(6, "main", "fix"), link(7, "fix", "more")];
        let stacks = chains(&links);
        assert_eq!(stacks.len(), 1);
        assert_eq!(numbers(&stacks[0]), vec![6, 7]);
    }

    #[test]
    fn a_fork_pr_can_still_sit_on_a_branch_here() {
        let mut fork = link(8, "a", "patch");
        fork.cross_repo = true;
        let stacks = chains(&[link(1, "main", "a"), fork]);
        assert_eq!(numbers(&stacks[0]), vec![1, 8]);
    }

    #[test]
    fn a_branch_point_ends_the_chain_and_starts_one_per_child() {
        let links = vec![
            link(1, "main", "a"),
            link(2, "a", "b"),
            link(3, "b", "c1"),
            link(4, "b", "c2"),
            link(5, "c1", "d"),
        ];
        let stacks = chains(&links);
        let found: Vec<_> = stacks.iter().map(numbers).collect();
        assert_eq!(found, vec![vec![1, 2], vec![3, 5]]);
        assert_eq!(stacks[1].root, "b");
    }

    #[test]
    fn a_missing_middle_pr_splits_the_chain() {
        // The caller decides whether the links are complete; the chains only follow them.
        let links = vec![
            link(1, "main", "a"),
            link(2, "a", "b"),
            link(4, "c", "d"),
            link(5, "d", "e"),
        ];
        let found: Vec<_> = chains(&links).iter().map(numbers).collect();
        assert_eq!(found, vec![vec![1, 2], vec![4, 5]]);
    }

    #[test]
    fn the_index_places_each_member_in_its_repo() {
        let mut index = StackIndex::default();
        index.add_repo("acme/app", &[link(1, "main", "a"), link(2, "a", "b")], true);
        index.add_repo("acme/lib", &[link(1, "main", "x")], false);
        let place = index.place("acme/app", 2).unwrap();
        assert_eq!(place.position, 2);
        assert_eq!(index.stacks[place.stack].key(), ("acme/app".to_string(), 1));
        assert_eq!(index.place("acme/lib", 1), None, "same number, other repo");
    }

    #[test]
    fn a_page_of_open_prs_parses_with_its_cursor() {
        let page = serde_json::json!({
            "pageInfo": {"hasNextPage": true, "endCursor": "abc"},
            "nodes": [{
                "number": 4, "title": "Add auth", "baseRefName": "main", "headRefName": "auth",
                "isCrossRepository": false, "isDraft": true, "reviewDecision": "APPROVED"
            }]
        });
        let (links, next) = parse_links_page(&page).unwrap();
        assert_eq!(next.as_deref(), Some("abc"));
        assert_eq!(links[0].head, "auth");
        assert!(links[0].is_draft);
        assert_eq!(links[0].review, ReviewState::Approved);
        let last =
            serde_json::json!({"pageInfo": {"hasNextPage": false, "endCursor": "z"}, "nodes": []});
        assert_eq!(parse_links_page(&last).unwrap().1, None);
    }

    #[test]
    fn the_window_keeps_both_ends_and_two_neighbors() {
        use Slot::{Gap, Member};
        assert_eq!(
            window(10, 1, false),
            vec![Member(0), Member(1), Member(2), Gap(6), Member(9)]
        );
        assert_eq!(
            window(10, 5, false),
            vec![
                Member(0),
                Gap(1),
                Member(2),
                Member(3),
                Member(4),
                Member(5),
                Member(6),
                Gap(2),
                Member(9)
            ]
        );
        assert_eq!(
            window(10, 10, false),
            vec![Member(0), Gap(6), Member(7), Member(8), Member(9)]
        );
        assert_eq!(window(10, 5, true).len(), 10);
        assert_eq!(window(3, 2, false).len(), 3);
    }
}

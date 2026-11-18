# How it works

## GitHub access

revu talks to GitHub only through the GitHub CLI, with `gh api graphql`. It stores no token of its own, so it has exactly the access your `gh` login has.

## What revu writes to GitHub

revu reads by default. It writes only when you press a key that asks it to:

| Key | What it writes |
|---|---|
| `R` | A review: Comment, Approve or Request changes, with your pending line comments |
| `c` | A line comment in your pending review, which only you see until you submit it |
| `X` | Deletes your pending review and its line comments |
| `e` | Adds or removes labels |
| `v` | The Viewed checkbox on a file |

GitHub keeps one pending review per person per pull request, so line comments you start in the browser and in revu land in the same review.

## Local clones

The diffs come from git, not from GitHub's API, so they need the pull request's commits in a local clone.

1. revu looks in each `workspace_dir` folder, in order, for the folder named after the repository.
2. Then it looks inside each of them for any folder whose `origin` is that repository. Only folders directly inside are checked, not deeper ones.
3. If there is none, it asks before cloning into the first folder, with `gh repo clone`.
4. If the commits are missing, it fetches the pull request into `refs/revu/pr/<number>/` only, with `--no-write-fetch-head`.

revu never checks out a branch or touches your working tree, the index or `FETCH_HEAD`, so it is safe to run beside an editor or another tool in the same clone.

## The overview

The merge box says whether the pull request can merge, and counts failing and pending checks, unresolved review threads and pending approvals. Skipped checks count neither as passed nor failed.

The sidebar counts what others did since your last comment or review. If you have not commented, it counts everything since the pull request opened. It lists every reviewer with their state; a member who reviewed for a team sits under the team. Assignees, labels, the milestone and linked issues show only when set.

## The API budget

The top line shows what is left of GitHub's API budget, such as `API 4676 left, resets 19:23`, from the headers of the latest call. It turns yellow under 20% and red under 5%.

GitHub keeps more than one counter and reports them at random. revu shows the lowest one, and never lets the number jump back up before its reset time.

The label menu loads a repository's labels once per pull request you open, not on every keystroke.

## When something fails

When a call to GitHub fails, revu asks `gh auth status` why. A working setup never pays for this check.

- If gh is missing, logged out, or cannot reach GitHub, a full screen says which, with the fix and the time of the last check. `r` checks again. It works the same from the inbox or from a pull request.
- If the failure is already a network error, such as a timeout, revu shows that screen at once, without waiting for a second timeout.
- A rate limit says when it resets, from the failed response itself.
- Any other failure that empties every inbox group, such as a GitHub outage, shows GitHub's error in place of the groups.
- Without git, diffs, find and `s` say git is missing and how to install it.

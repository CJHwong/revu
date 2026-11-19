# How it works

## GitHub access

revu talks to GitHub only through the GitHub CLI, with `gh api graphql`. It stores no token of its own, so it has exactly the access your `gh` login has.

## What revu writes to GitHub

revu reads by default. It writes only when you press a key that asks it to:

| Key | What it writes |
|---|---|
| `R` | A review: Comment, Approve or Request changes, with your pending line comments |
| `S`, then `Enter` | A line comment in your pending review, which only you see until you submit it |
| `X` | Deletes your pending review and its line comments |
| `M` | Merges the pull request, or turns auto-merge on or off |
| `e` | Adds or removes labels |
| `v` | The Viewed checkbox on a file |

A merge names the head commit revu showed you. If someone pushed after that, GitHub refuses the merge with "Head branch was modified"; press `r` and look again. A review revu starts is pinned to that commit too, so an approval covers the code you read, not commits pushed since.

GitHub keeps one pending review per person per pull request, so line comments you start in the browser and in revu land in the same review.

## Local clones

The diffs come from git, not from GitHub's API, so they need the pull request's commits in a local clone.

1. revu looks in each `workspace_dir` folder, in order, for the folder named after the repository.
2. Then it looks inside each of them for any folder whose `origin` is that repository. Only folders directly inside are checked, not deeper ones.
3. If there is none, it asks before cloning into the first folder, with `gh repo clone`.
4. If the commits are missing, it fetches the pull request into `refs/revu/pr/<number>/` only, with `--no-write-fetch-head`.

revu never checks out a branch or touches your working tree, the index or `FETCH_HEAD`, so it is safe to run beside an editor or another tool in the same clone.

## The inbox

Each row shows the title, the review state, the checks and the comment count. Its second line shows the repository, the author, the last update and the size of the diff, such as `+120 -34, 7 files`.

A `●` before the title marks a pull request with new commits or new comments since you last left it. revu saves each pull request's head commit and comment count when you open it and again when you go back to the inbox, so your own comments do not mark it. A pull request you have never opened has no mark.

The second line ends with who the pull request waits on:

| Label | When |
|---|---|
| Your turn | Someone asked you by name for a review, or pushed after your last review. A request to one of your teams does not count. On your own pull request: a reviewer requested changes, or it has conflicts. |
| Author's turn | You requested changes and nothing was pushed since. |
| Conflicts | GitHub found merge conflicts. GitHub checks this lazily, so a pull request it has not checked yet shows nothing. |

In a terminal at least 160 columns wide, the right 40% of the inbox previews the selected pull request: its branches, whether it can merge, the reviewers, the labels and the description. revu fetches the preview once the cursor rests on a row for 300 ms, so moving through the list sends no request per row. It fetches again when the row shows a newer update. A failed preview tries again on `r`. Each preview is one GraphQL call, 1 point of the API budget. Opening a pull request whose preview is current shows that preview at once and skips fetching it again; `r` in the pull request fetches everything.

## Edit at the PR's head

When `o` opens a file in your editor and the clone is on another branch, revu adds a worktree of the clone at the PR's head, detached, under `$XDG_CACHE_HOME/revu/worktrees/<owner>/<repo>/<number>` (else `~/.cache/revu/...`). Your clone's branch and working tree stay as they are. The worktree shares the clone's objects, so it costs only the checked-out files.

The next `o` on the same pull request reuses the worktree and moves it to the PR's newest head. If it holds changes you have not committed, revu leaves it where it is and says so, so your edits are never overwritten. To keep an edit, commit it in the worktree, on a branch you create there. To drop a worktree, run `git worktree remove <folder>` in the clone.

## The overview

The merge box says whether the pull request can merge, and counts failing and pending checks, unresolved review threads and pending approvals. Skipped checks count neither as passed nor failed.

A review thread shows the last four lines of the diff it is on, ending at the commented line, as GitHub's conversation tab does. A removed line shows its old line number. The code comes in a second call after the comments show, since on a big pull request it can be larger than the whole conversation.

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
- Without git, diffs and find say git is missing and how to install it.

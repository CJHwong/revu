# How it works

## GitHub access

revu talks to GitHub only through the GitHub CLI, with `gh api graphql`. It stores no token of its own, so it has exactly the access your `gh` login has.

## What revu writes to GitHub

revu reads by default. It writes only when you press a key that asks it to:

| Key | What it writes |
|---|---|
| `R` | A review: Comment, Approve or Request changes, with your pending line comments |
| `S`, then `Enter` | A line comment in your pending review, which only you see until you submit it; on a line with a pending comment, an edit to it |
| `S`, then `x` | Deletes your pending comment on the line |
| `X` | Deletes your pending review and its line comments |
| `M` | Merges the pull request, turns auto-merge on or off or changes its method, or switches the pull request between draft and ready for review |
| `e` | Adds or removes labels |
| `v` | The Viewed checkbox on a file |

A merge names the head commit revu showed you. If someone pushed after that, GitHub refuses the merge with "Head branch was modified"; press `r` and look again. A review revu starts is pinned to that commit too, so an approval covers the code you read, not commits pushed since.

GitHub keeps one pending review per person per pull request, so line comments you start in the browser and in revu land in the same review. revu asks GitHub for your pending review before each comment and each review it sends, because the review can change outside revu: GitHub deletes it with its last comment, and you may submit or discard it in the browser.

## Local clones

The diffs come from git, not from GitHub's API, so they need the pull request's commits in a local clone.

1. revu looks in each `workspace_dir` folder, in order, for the folder named after the repository.
2. Then it looks inside each of them for any folder whose `origin` is that repository. Only folders directly inside are checked, not deeper ones.
3. If there is none, it asks before cloning into the first folder, with `gh repo clone`.
4. If the commits are missing, it fetches the pull request into `refs/revu/pr/<number>/` only, with `--no-write-fetch-head`.

revu never checks out a branch or touches your working tree, the index or `FETCH_HEAD`, so it is safe to run beside an editor or another tool in the same clone.

## The inbox

Each row shows the title, the review state, the checks and the comment count. Its second line shows the repository, the author, the last update and the size of the diff, such as `+120 -34, 7 files`. Each item keeps its column on every row. When the second line does not fit, the last update goes first, then the author; only a repository name too long alone is cut. In an inbox list under 120 columns wide, the file count goes and the comment count shows as `27 ✎`.

A `●` before the title marks a pull request with new commits or new comments since you last left it. revu saves each pull request's head commit and comment count when you open it and again when you go back to the inbox. After you comment, review or reply, the next inbox fetch counts as seen too, so your own comments do not mark it, even when you go back before the pull request reloads. A pull request you have never opened has no mark.

Under the review state, the second line says who the pull request waits on, and under the checks, whether it has conflicts:

| Label | When |
|---|---|
| Your turn | Someone asked you by name for a review, or pushed after your last review. A request to one of your teams does not count. Your last review is your latest approval or change request, so a reply in a thread later does not replace it, and a push means the pull request's head moved past the commit you reviewed. On your own pull request: a reviewer requested changes, or it has conflicts. |
| Author's turn | You requested changes and nothing was pushed since. |
| Conflicts | GitHub found merge conflicts. GitHub checks this lazily, so a pull request it has not checked yet shows nothing. |

In a terminal at least 160 columns wide, the right 40% of the inbox previews the selected pull request: its branches, whether it can merge, the reviewers, the labels and the description. revu fetches the preview once the cursor rests on a row for 300 ms, so moving through the list sends no request per row. It fetches again when the row shows a newer update, a new head commit, other checks or another merge state, such as after a push to the base branch. A failed preview tries again on `r`. Each preview is one GraphQL call, 1 point of the API budget. Opening a pull request whose preview is current shows that preview at once and skips fetching it again; `r` in the pull request fetches everything.

## Stacked pull requests

A pull request sits on top of another when its base branch is the other one's head branch, in the same repository. A chain of two or more is a stack. revu links them by branch name only. A pull request from a fork never counts as the one below another.

In a group, the pull requests of one stack fold into one row, at the place of the newest one. The row shows the branch the stack starts on and the repository. It also counts the group's share of the stack, such as `3 of 10 PRs here`, and how many wait on you. `Enter` or `Space` unfolds the row. The pull requests then show in stack order, bottom first, each with its place, such as `4/10`. A stack stays unfolded across refreshes. A filter unfolds every stack, so a match never hides in a folded one. A pull request alone from its stack in a group keeps a row of its own, with its place.

The Overview of a pull request in a stack has a Stack card under the merge box. It shows the branch the stack starts on, the bottom and the top pull request, and the two on each side of this one, each with its state. The others fold into a line such as `… 3 more`. Links come only from open pull requests, so a merged one leaves the chain.

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

To link stacks, revu fetches the open pull requests of a repository: 100 per call, with only their branches, titles and states. Each call costs 1 point, so a repository with 156 open pull requests costs 2, however deep its stacks are. revu asks once per inbox refresh, for each repository with an inbox row based on a branch other than the default one, and for the pull request you open. A repository with more than 1,000 open pull requests links only the newest 1,000.

Until the first answer arrives, a stack row says `linking...` and shows no places, because the loaded rows alone may miss a pull request in the middle. A refresh keeps the last answer until the next one arrives. If a call fails, the footer says why. A stack with no earlier answer then says `linked from the inbox only`.

## When something fails

When a call to GitHub fails, revu asks `gh auth status` why. A working setup never pays for this check.

- If gh is missing, logged out, or cannot reach GitHub, a full screen says which, with the fix and the time of the last check. `r` checks again. It works the same from the inbox or from a pull request.
- If the failure is already a network error, such as a timeout, revu shows that screen at once, without waiting for a second timeout.
- A rate limit says when it resets, from the failed response itself.
- Any other failure that empties every inbox group, such as a GitHub outage, shows GitHub's error in place of the groups.
- Without git, diffs and find say git is missing and how to install it.

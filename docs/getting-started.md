# Getting started

This walks through a first session: install revu, open a pull request, read its diff, and send a review. It takes about ten minutes.

## 1. Log in to GitHub

revu reads GitHub through the GitHub CLI, so install `gh` and log in first:

```bash
gh auth login
```

## 2. Install and start revu

```bash
cargo install --git https://github.com/CJHwong/revu
revu
```

The inbox opens. It shows the pull requests that ask for your review or your team's, your drafts, your own that wait on others, and the ones you reviewed, in groups like GitHub's own inbox. Move with `j` and `k`, and fold a group with `Space`.

The first run also writes `~/.config/revu/config.yaml`. You change it in step 4.

## 3. Open a pull request

Select a pull request and press `Enter`. The Overview shows:

- the description, then the comments and reviews with their threads
- a merge box that says whether the pull request can merge, with its checks, unresolved threads and approvals
- a sidebar with what changed since you last commented, and every reviewer's state

Press `j` and `k` to move from comment to comment, and `Enter` to fold one. Press `1` to `4` to switch between Overview, Commits, Checks and Files changed.

To open a pull request that is not in your inbox, press `Ctrl+L` and type its link or `owner/repo#123`.

## 4. Point revu at your clones

Press `4` for Files changed. The diffs come from your local clone of the repository, so revu asks once for the folder that holds your clones, such as `~/code`, and saves it as `workspace_dir`. If the repository is not cloned there, revu offers to clone it.

## 5. Read the diff

- `j` and `k` pick a file. `J` and `K` scroll its diff.
- `m` switches between word diff, delta and difftastic. revu skips the ones you have not installed.
- `/` finds text in every changed file. `Tab` completes a word or file name from the diff, and `n` goes to the next match.
- `v` marks a file viewed on GitHub, and `]` goes to the next unviewed file.

## 6. Comment on a line

On Files changed, press `c` for comment mode. It shows the selected file's diff with line numbers.

1. Move to a line with `j` and `k`. Press `V` to mark a range.
2. Press `Enter`, write the comment, and press `ctrl+s` to save it.

The comment joins your pending review on GitHub. Only you see it until you submit the review.

## 7. Submit the review

Press `R`. `Tab` picks Comment, Approve or Request changes. Write a summary, then press `ctrl+s` and `y` to send it with your line comments.

## Next

- [How-to guides](how-to.md) to set up your own inbox groups, a theme, or another diff view.
- [Keys](keys.md) for every key. Press `?` in the app for the same list.

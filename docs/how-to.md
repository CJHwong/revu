# How-to guides

Short recipes for common tasks. Each one assumes revu already runs; see [Getting started](getting-started.md) if it does not.

## Set up your inbox groups

Each group is one GitHub search. revu starts with five built-in groups. To set your own, uncomment `groups` in `~/.config/revu/config.yaml` and edit it. Your list replaces the built-in one:

```yaml
groups:
  - title: Needs your review
    query: user-review-requested:@me
  - title: My team's repo
    query: repo:acme/api
    collapsed: true
```

1. Write the query as you would in GitHub's search box. revu adds `is:pr is:open archived:false`, and `sort:updated-desc` unless the query has its own `sort:`.
2. Press `r` in the inbox to reload the config.

To see older pull requests too, remove `updated_within_days`, or change it under Settings (`,`): leave it empty for no limit.

## Open a pull request that is not in your inbox

Press `Ctrl+L`, or paste, and give its link, `owner/repo#123`, or `repo#123` for a repo in your inbox or your clones. `Tab` completes the repo name.

## Change the theme

1. Press `,` and pick Theme.
2. Move to a theme and press `Enter`. It applies at once, diffs included, and saves to the config.

`terminal` uses your terminal's own colors. See [Configuration](configuration.md#theme) for every theme.

## Keep clones in more than one folder

1. Press `,` and pick Workspace folders.
2. Type the folders with commas between them, such as `~/Workspace, ~/Workspace/side_projects`, and press `Enter`.

revu searches them in order and clones new repositories into the first. See [Configuration](configuration.md#workspace_dir) to write the list in the config instead.

## Add a diff view

Add an entry to `diff_renderers` in the config. The command runs inside the clone:

```yaml
diff_renderers:
  - name: side-by-side
    requires: delta
    install: brew install git-delta
    command: git diff {base}...{head} -- {paths} | delta --side-by-side --paging=never --width={width}
```

`m` now includes it. If `requires` is not on your PATH, revu leaves it out and says how to install it. See [Configuration](configuration.md#diff_renderers) for every placeholder.

Note: the list replaces the default one, so copy the defaults in too if you want to keep them.

## Review a pull request

1. On Files changed, press `S` on a file to select lines. Move with `j`/`k`, press `V` for a range, and `Enter` to write a comment. `ctrl+s` saves it to your pending review.
2. Press `R`, pick Comment, Approve or Request changes with `Tab`, write a summary, and press `ctrl+s`, then `y`.

To throw away the pending review and its line comments instead, press `X`, then `y`.

On your own pull request, and on one that is merged or closed, the review takes only Comment.

## Review a stack of pull requests

1. In the inbox, press `Enter` on the stack's row. Its pull requests list bottom first, each with its place, such as `1/10`.
2. Open the bottom one and review it.
3. Press `}` to open the next one up, or `{` for the one below. On the Overview, the Stack card shows where you are.

See [Stacked pull requests](how-it-works.md#stacked-pull-requests).

## Hand a spot to a coding agent

1. Put the cursor where the question is. In select lines (`S`), that is a line, a `V` range, or a thread. On any tab, it is the pull request as a whole; on Files changed, also the selected file.
2. Press `a`. The box lists what revu attaches. Write what you want the agent to do. `Tab` completes the word before the cursor from the diff's words and file names, shown dim ahead of it.
3. Press `ctrl+s`, pick where it goes with `j`/`k`, and press `Enter`. `Esc` goes back to the box.

The list offers, in order:

- **Claude Code in tmux**: a pane in your clone's folder that runs Claude Code. revu pastes the text into its prompt and takes you to the pane. You press `Enter` there, as a dialog open in the pane would take revu's `Enter` as a yes.
- **Codex session**: a Codex session started in your clone in the last day. revu queues the text with `codex queue`; the session runs it after its current turn.
- **New Claude Code session** and **New Codex session**: start in your clone with the text as the prompt. Inside tmux, in a new tmux window. Outside tmux, in a new tab of Ghostty, iTerm2, WezTerm, kitty, GNOME Terminal or Konsole, or a new Terminal window on macOS. In any other terminal, in this one until you quit the agent. kitty needs `allow_remote_control` in its config.
- **Copy to clipboard**: paste it into any agent.

The list also shows the commands your config adds under [`handoff_targets`](configuration.md#handoff_targets). A command that leaves out `{message}` runs on the context alone, so the box may be empty for it.

The agent gets the PR, its base and head, your clone, then the lines in the same Markdown that `y` copies, and the thread, then your request. When the clone is on another branch, it also says how to read the PR's code with `git show` and `git diff`.

If a send fails, revu copies the text to the clipboard and says why, unless the target took the context alone: then it shows the error and copies nothing. The agent sees the clone's working tree, which is the PR's code only when its branch is checked out; the box says when the clone is on another branch. The first time an agent opens a folder, it may ask whether you trust it.

## Merge a pull request

1. Press `M` on any tab of the pull request.
2. Pick Create a merge commit, Squash and merge or Rebase and merge with `j`/`k`, and press `Enter`, then `y`.

If GitHub blocks the merge, for a missing approval or a check, the same steps turn on auto-merge, so GitHub merges it once the requirements pass. An admin can tick Bypass requirements first to merge at once. Press `M` again to turn auto-merge off.

## Change labels

On the Overview, press `e`. Type to filter, `Space` ticks or unticks, and `Enter` applies the change to GitHub.

## Search code outside the diff

On Files changed, press `/`, type the text, and press `Enter`. `n` goes through the matches in the changed files first. After the last one, it goes on through every other file in the repository at the pull request's head, opening each at its match. Past the last repo match, `n` comes back to the diff. `N` goes the other way. `Esc` closes a file and goes back to the diff.

## Copy text

- On Files changed, `y` copies the selected file's path. In select lines (`S`), `y` copies the line or the `V` range as a Markdown diff snippet with its path and line numbers. See [Keys](keys.md#copy-lines).
- In a review or comment box, `ctrl+c` copies the selection, or all the text when nothing is selected. Inside tmux, add `set -g set-clipboard on` to `~/.tmux.conf`.
- Anywhere else, hold your terminal's bypass modifier while you select with the mouse: Shift in most terminals, Option or Fn in some. revu captures the mouse, so a plain drag does not select.

## Remove the refs left by an older version

Versions from before the rename to revu fetched pull requests into `refs/rs-github-inbox/pr/`. revu no longer uses them. Run this inside each clone to delete them:

```bash
git for-each-ref --format='delete %(refname)' refs/rs-github-inbox/ | git update-ref --stdin
```

## Fix common errors

| What you see | What to do |
|---|---|
| "The GitHub CLI is not installed" | Install `gh`, run `gh auth login`, then press `r`. |
| "The GitHub CLI is not logged in" | Run `gh auth login`, then press `r`. |
| "Cannot reach GitHub" | Check the network, a VPN, or `HTTPS_PROXY`, then press `r`. |
| "GitHub's API limit is used up" | Wait until the reset time it shows, then press `r`. |
| "delta is not installed, so m skips it" | Install it with the command shown, or ignore it: the other views still work. |
| "git is not installed" | Install git. Diffs, find and search need it. |

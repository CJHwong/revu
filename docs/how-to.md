# How-to guides

Short recipes for common tasks. Each one assumes revu already runs; see [Getting started](getting-started.md) if it does not.

## Set up your inbox groups

Each group is one GitHub search. Edit `groups` in `~/.config/revu/config.yaml`:

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

To see older pull requests too, remove `updated_within_days`, or change it under Settings (`,`).

## Open a pull request that is not in your inbox

Press `Ctrl+L`, or paste, and give its link or `owner/repo#123`.

## Change the theme

1. Press `,` and pick Theme.
2. Move to a theme and press `Enter`. It applies at once, diffs included, and saves to the config.

`terminal` uses your terminal's own colors. See [Configuration](configuration.md#theme) for every theme.

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

1. On Files changed, press `c` on a file. Move with `j`/`k`, press `V` for a range, and `Enter` to write a comment. `ctrl+s` saves it to your pending review.
2. Press `R`, pick Comment, Approve or Request changes with `Tab`, write a summary, and press `ctrl+s`, then `y`.

To throw away the pending review and its line comments instead, press `X`, then `y`.

On your own pull request, and on one that is merged or closed, the review takes only Comment.

## Change labels

On the Overview, press `e`. Type to filter, `Space` ticks or unticks, and `Enter` applies the change to GitHub.

## Search code outside the diff

Press `s` on any pull request tab. It searches every file in the repository at the pull request's head, ignoring case. `Enter` on a match opens the file at that line.

## Copy text

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

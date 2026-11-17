# rs-github-inbox

A terminal version of GitHub's pull request inbox. It shows the pull requests you care about across every repository, grouped like the web inbox, and opens each one with its rendered description, reviewers, labels, checks and activity. It writes one thing to GitHub: the Viewed checkbox on a file, when you press `v`.

## Requirements

- The GitHub CLI (`gh`), logged in. The tool calls `gh api graphql` and stores no token of its own.
- Rust, to build it.

## Install

```bash
cargo install --path .
rs-github-inbox
```

## Local clones

The diff panes need the PR's commits locally. The tool looks for your clone in `workspace_dir`: first the folder named after the repo, then any folder whose `origin` is that repo. If there is none, it asks before cloning. It fetches the PR into `refs/rs-github-inbox/pr/<number>/` only, with `--no-write-fetch-head`, and never checks out a branch or touches your working tree.

## Configuration

The first run writes `~/.config/rs-github-inbox/config.yaml` (or `$XDG_CONFIG_HOME/rs-github-inbox/config.yaml`). Press `r` in the inbox to reload it.

- `groups`: one GitHub search per group, in display order. The tool adds `is:pr is:open archived:false` to each query. `collapsed: true` folds the group on start.
- `updated_within_days`: adds `updated:>=<date>` to every query. Remove the key to see older pull requests.
- `theme`: `dracula` (the default) or `terminal`, which uses your terminal's own colors.
- `workspace_dir`: the folder that holds your clones. The Files changed tab asks for it once and appends it here.
- `diff_renderers`: the diff views that `m` switches between, as shell commands run inside the clone. `{base}`, `{head}`, `{paths}` and `{width}` are replaced and quoted for the shell. The defaults are `word` (`git diff --word-diff`), `delta` and `difftastic`.
- `open_diff`: the shell command that `d` runs. `{repo}`, `{number}` and `{url}` are replaced. The default is `gh pr diff {number} -R {repo} | delta`.

A search shows at most 100 pull requests. A group with more matches shows a count like `100 of 140`.

## Keys

| Screen | Key | Action |
|---|---|---|
| Anywhere | `Ctrl+L` (or `Cmd+L`, see below) | Open a pull request by link or `owner/repo#123` |
| Anywhere | paste | Opens the same link box with the pasted text |
| Inbox | `j` / `k` | Move |
| Inbox | `Enter` | Open the pull request, or fold a group header |
| Inbox | `Space` | Fold the current group |
| Inbox | `g` / `G` | Top / bottom |
| Inbox | `r` | Reload the config and fetch again |
| Pull request | `Tab` / `Shift+Tab`, `1`-`4` | Switch between Overview, Commits, Checks and Files changed |
| Files changed | `v` | Toggle Viewed. It syncs to GitHub's Viewed checkbox at once. |
| Files changed | `H` | Hide viewed files |
| Files changed, Commits | `J` / `K`, `Space` / `b` | Scroll the diff |
| Files changed, Commits | `m` | Switch the diff renderer |
| Files changed, Commits | `/`, then `n` / `N` | Find text in the diff, next and previous match |
| Files changed | `e` | Show the whole file at the PR head |
| Pull request | `s` | Search every file in the repo at the PR head, not only the changed ones. `Enter` opens the file at the match. |
| Pull request | `j` / `k`, `Space` / `b` | Scroll the description, or move in a list |
| Pull request | `Enter` | Open the selected commit or check in the browser |
| Pull request | `r` | Fetch the pull request again |
| Pull request | `Esc` | Back to the inbox |
| Both | `o` | Open in the browser |
| Both | `d` | Run the `open_diff` command |
| Both | `q` | Quit from the inbox, back from a pull request |

`Cmd+L` works only in terminals that pass Cmd to applications through the kitty keyboard protocol and do not use `Cmd+L` themselves. `Ctrl+L` works everywhere.

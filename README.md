# rs-github-inbox

A terminal version of GitHub's pull request inbox. It shows the pull requests you care about across every repository, grouped like the web inbox, and opens each one with its rendered description, reviewers, labels, checks and activity. It is read-only.

## Requirements

- The GitHub CLI (`gh`), logged in. The tool calls `gh api graphql` and stores no token of its own.
- Rust, to build it.

## Install

```bash
cargo install --path .
rs-github-inbox
```

## Configuration

The first run writes `~/.config/rs-github-inbox/config.yaml` (or `$XDG_CONFIG_HOME/rs-github-inbox/config.yaml`). Press `r` in the inbox to reload it.

- `groups`: one GitHub search per group, in display order. The tool adds `is:pr is:open archived:false` to each query. `collapsed: true` folds the group on start.
- `updated_within_days`: adds `updated:>=<date>` to every query. Remove the key to see older pull requests.
- `theme`: `dracula` (the default) or `terminal`, which uses your terminal's own colors.
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
| Pull request | `Tab` / `Shift+Tab`, `1`-`3` | Switch between Overview, Commits and Checks |
| Pull request | `j` / `k`, `Space` / `b` | Scroll the description, or move in a list |
| Pull request | `Enter` | Open the selected commit or check in the browser |
| Pull request | `r` | Fetch the pull request again |
| Pull request | `Esc` | Back to the inbox |
| Both | `o` | Open in the browser |
| Both | `d` | Run the `open_diff` command |
| Both | `q` | Quit from the inbox, back from a pull request |

`Cmd+L` works only in terminals that pass Cmd to applications through the kitty keyboard protocol and do not use `Cmd+L` themselves. `Ctrl+L` works everywhere.

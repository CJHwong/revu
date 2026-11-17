# rs-github-inbox

A terminal version of GitHub's pull request inbox. It shows the pull requests you care about across every repository, grouped like the web inbox, and opens each one like GitHub's Overview: the description, the comments and reviews with their threads, folded `<details>` blocks, reviewers and labels. It writes one thing to GitHub: the Viewed checkbox on a file, when you press `v`.

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

- `groups`: one GitHub search per group, in display order. The tool adds `is:pr is:open archived:false` to each query, and `sort:updated-desc` unless the query has its own `sort:`. A group shows at most 100 PRs, the newest ones, and its header says "100 of N" when it has more. `collapsed: true` folds the group on start.
- `updated_within_days`: adds `updated:>=<date>` to every query. Remove the key to see older pull requests.
- `theme`: `dracula` (the default) or `terminal`, which uses your terminal's own colors.
- `workspace_dir`: the folder that holds your clones. The Files changed tab asks for it once and appends it here.
- `diff_renderers`: the diff views that `m` switches between, as shell commands run inside the clone. `{base}`, `{head}`, `{paths}` and `{width}` are replaced and quoted for the shell. `{syntax_theme}`, `{added_bg}` and `{removed_bg}` come from the app's theme; the default `delta` command passes them, so all three renderers share one set of syntax colors and change tints, and follow a theme change. The defaults are `word` (`git diff --word-diff`), `delta` and `difftastic`. `highlight: true` on a renderer adds syntax colors from `bat`, and shows the renderer's red and green as a background tint. It is on for `word` and `difftastic`, which color little of the code themselves. Without `bat`, the output shows as the renderer drew it.
- `open_diff`: the shell command that `d` runs. `{repo}`, `{number}` and `{url}` are replaced. The default is `gh pr diff {number} -R {repo} | delta`.

A search shows at most 100 pull requests. A group with more matches shows a count like `100 of 140`.

## Keys

Press `?` anywhere for this list.

| Where | Key | Action |
|---|---|---|
| Anywhere | `Ctrl+L` (or `Cmd+L`, see below), or paste | Open a pull request by link or `owner/repo#123` |
| Anywhere | `?` | Show every key. Any key closes the list. |
| Anywhere | `T` | Pick a color theme: `dracula`, `terminal` (your terminal's own colors), `catppuccin_mocha`, `catppuccin_macchiato`, `catppuccin_frappe`, `nord`, `gruvbox_dark`, `solarized_dark`, `one_half_dark`, `monokai`, or the light `catppuccin_latte`, `gruvbox_light`, `solarized_light`, `one_half_light` and `github`. Each pairs with the bat theme of the same name, so code in diffs matches the page. `Enter` applies it at once, diffs included, and saves it as the `theme:` line in the config. |
| Inbox | `j` / `k`, `g` / `G` | Move, top, bottom |
| Inbox | `Enter`, `l` | Open the pull request, or fold a group header |
| Inbox | `Space` | Fold the current group |
| Inbox | `Tab` / `Shift+Tab` | Next or previous group |
| Inbox | `r` | Reload the config and fetch again |
| Inbox | `q` | Quit |
| Pull request | `1`-`4`, `Tab` / `Shift+Tab` | Switch between Overview, Commits, Checks and Files changed. The tabs show GitHub's counts, not the digits. |
| Pull request | `Esc`, `h`, `q` | Back to the inbox |
| Pull request | `g` / `G`, `Space` / `b` | Top, bottom, page down, page up |
| Pull request | `s` | Search every file in the repo at the PR head, not only the changed ones. `Enter` opens the file at the match, with syntax colors from `bat` when it is installed. It needs the local clone, which the Files changed or Commits tab finds first. |
| Pull request | `r` | Fetch the pull request again |
| Overview | `j` / `k` | Next or previous comment, review thread or folded block. Inside one taller than the screen, a third of a screen. The cursor never stops on an empty line. |
| Overview | `Enter` | Open or close the card, review thread or `<details>` block under the cursor |
| Overview | `]` / `[` | Next or previous card or folded block |
| Overview | `f` / `F` / `n` | Filter the comments (All, Unresolved, Mine), flip oldest or newest first, hide bots. Mine shows only your own words, with the threads you replied in open. |
| Files changed | `Enter` | Show the whole file with its changes marked, drawn by the current renderer, with syntax colors. On a folder, fold or open it. |
| Files changed | `t` | Switch the list between a folder tree and flat paths |
| Files changed | `z` | Hide or show the list, so the diff gets the full width |
| Files changed | `v` | Toggle Viewed. It syncs to GitHub's Viewed checkbox at once. |
| Files changed | `H` | Hide viewed files |
| Files changed | `]` / `[` | Next or previous unviewed file |
| Files changed, Commits | `J` / `K`, `Space` / `b` | Scroll the diff |
| Files changed, Commits | `m` | Switch the diff renderer |
| Files changed, Commits | `/`, then `n` / `N` | Find text in the diff, next and previous match |
| Commits, Checks | `Enter` | Open the selected commit or check in the browser |
| Inbox, pull request | `o` | Open in the browser |
| Inbox, pull request | `d` | Run the `open_diff` command |

`Cmd+L` works only in terminals that pass Cmd to applications through the kitty keyboard protocol and do not use `Cmd+L` themselves. `Ctrl+L` works everywhere.

The top line shows what is left of GitHub's API budget (`API: 4676/5000 left, resets 19:23`), from the headers of the latest call. It turns yellow under 20% and red under 5%. A click works like the keys on what it hits: it opens a pull request in the inbox, switches a tab, selects a file, commit, check or search hit, folds a folder, and opens or closes a card, thread or folded block on the Overview. The mouse wheel scrolls the pane under the pointer: the diff or the list beside it. On the Overview one tick goes to the next card, or a third of a screen inside a card taller than the screen. The app captures the mouse for this, so selecting text needs the terminal's bypass modifier (Shift in most terminals, Option or Fn in some).

# Keys

Every key, by where it works. Press `?` anywhere in revu for the same list.

## Anywhere

| Key | Action |
|---|---|
| `Ctrl+L` (or `Cmd+L`), or paste | Open a pull request by link or `owner/repo#123`. `Cmd+L` works only in terminals that pass Cmd to applications through the kitty keyboard protocol and do not use `Cmd+L` themselves. `Ctrl+L` works everywhere. |
| `?` | Show every key. `j`/`k` and `Space`/`b` scroll it; any other key closes it. |
| `,` | Settings: the theme, the workspace folders, and how many days back the inbox looks. `Enter` edits the selected one and saves it to its line in the config at once; comments and other lines stay as they are. A new number of days reloads the inbox. |
| `,`, then Theme | Pick a color theme. `Enter` applies it at once, diffs included, and saves it as the `theme:` line in the config. See [Configuration](configuration.md#theme) for the list. |

## Inbox

| Key | Action |
|---|---|
| `j` / `k`, `g` / `G` | Move, top, bottom |
| `Enter`, `l` | Open the pull request, or fold a group header |
| `Space` | Fold the current group |
| `Tab` / `Shift+Tab` | Next or previous group |
| `r` | Reload the config and fetch again |
| `o` | Open in the browser |
| `d` | Run the `open_diff` command |
| `q` | Quit |

## Pull request

| Key | Action |
|---|---|
| `1`-`4`, `Tab` / `Shift+Tab` | Switch between Overview, Commits, Checks and Files changed. The tabs show GitHub's counts, not the digits. |
| `Esc`, `h`, `q` | Back to the inbox |
| `g` / `G`, `Space` / `b` | Top, bottom, page down, page up |
| `s` | Search every file in the repo at the PR head, not only the changed ones. `Tab` completes a word or file name from the PR's changes, as in find. `Enter` opens the file at the match, with syntax colors from `bat` when it is installed. It needs the local clone, which the Files changed or Commits tab finds first. |
| `r` | Fetch the pull request again |
| `R` | Review: `Tab` picks Comment, Approve or Request changes. `ctrl+s` asks to confirm and `y` sends. Your pending line comments go with it. Request changes needs a comment; Comment needs a comment or line comments. Only Comment applies on your own PR, as GitHub requires, and on a merged or closed one. |
| `X` | Discard your pending review and every line comment in it, after a `y`. |
| `o` | Open in the browser. On Files changed, GitHub's diff opens at the selected file, and in comment mode at the cursor's line. |
| `d` | Run the `open_diff` command |

## Overview

| Key | Action |
|---|---|
| `j` / `k` | Next or previous comment, review thread or folded block when its top is on screen. Otherwise a third of a screen, so a long comment is read through, not skipped. The cursor never stops on an empty line. |
| `Enter` | Open or close the card, review thread or `<details>` block under the cursor. On the merge box, `Enter` on the Checks row opens the Checks tab, and on the Comments row shows only unresolved threads. |
| `]` / `[` | Next or previous card or folded block |
| `f` / `F` / `n` | Filter the comments (All, Unresolved, Mine), flip oldest or newest first, hide bots. Mine shows only your own words, with the threads you replied in open. |
| `e` | Labels: type to filter, `↑`/`↓` move, `Space` ticks, `Enter` applies the changes to GitHub. |

## Files changed

A file with review threads shows their count, such as `●3`, and the diff pane's title lists their lines.

| Key | Action |
|---|---|
| `Enter` | Show the whole file with its changes marked, drawn by the current renderer, with syntax colors. On a folder, fold or open it. |
| `t` | Switch the list between a folder tree and flat paths |
| `z` | Hide or show the list, so the diff gets the full width |
| `y` | Copy the selected file's path, or the folder's path with a trailing `/`. |
| `v` | Toggle Viewed. It syncs to GitHub's Viewed checkbox at once. On a folder, it marks every file under it viewed, or unmarks them all when every one is viewed already. |
| `V` | Hide viewed files |
| `]` / `[` | Next or previous unviewed file |
| `c` | Comment mode on the selected file: its plain diff with line numbers. `j`/`k` move, `V` marks a range (on one side only, as GitHub requires), `Enter` writes a comment, `y` copies the line or range, `esc` goes back. The comment joins your pending review on GitHub, which only you see until `R` submits it; a dot marks lines it already covers. |

## Files changed and Commits

| Key | Action |
|---|---|
| `J` / `K`, `Space` / `b` | Scroll the diff |
| `m` | Switch the diff renderer. Renderers that are not installed are skipped. |
| `w` | Wrap long lines (the default) or not. Unwrapped, `H` / `L` scroll the diff sideways, as `J` / `K` scroll it down and up. |
| `/`, then `n` / `N` | Find text, ignoring case. See [Find](#find). |

## Commits and Checks

| Key | Action |
|---|---|
| `Enter` | Open the selected commit or check in the browser |

## Copy lines

In comment mode (`c`), `y` copies the line under the cursor, or the range `V` marked, as Markdown: the path and line numbers, then the lines in a `diff` block with their `+` and `-` marks. A copied range can span removed and added lines. The numbers are the new file's, or the old file's when every line was removed:

````
src/app.rs:40-42
```diff
   let name = repo.split_once('/');
-  let named = workspace.join(name);
+  for workspace in workspaces {
```
````

## Find

`/` finds text in the diff, ignoring case.

- `^` at the start ties the text to where the code starts, after line numbers, the +/- mark and indentation, so `^def ` finds definitions. `$` at the end ties it to where the code ends. In difftastic, each column counts on its own. `\^` and `\$` find the characters themselves.
- While you type, the prompt offers the most used word of the PR's changed lines that starts with your text, or a changed file's path, name or folder. `Tab` takes it.
- Every match is marked, the current one stronger, and the pane title shows `/text  match 2 of 3 in file 5 of 63`.
- On Files changed, `n` past a file's last match goes on to the next file that holds the text, opening its folder. On Commits, it searches the commit on screen.
- `s` searches the whole repo instead.

## Text boxes

Reviews and comments are multi-line text boxes.

| Key | Action |
|---|---|
| `Enter` | New line |
| `shift`+arrows | Select |
| `alt`+arrows | Move by word |
| `ctrl+a` | Select everything |
| `ctrl+c`, `ctrl+x` | Copy, cut to the clipboard. With nothing selected, `ctrl+c` copies all the text. |
| paste | Replaces the selection |
| `ctrl+s` | Send |
| `esc` twice | Throw the text away |

Copying uses OSC 52 and `pbcopy`. Inside tmux, OSC 52 needs `set -g set-clipboard on`.

## Mouse

A click works like the keys on what it hits: it opens a pull request in the inbox, switches a tab, selects a file, commit, check or search hit, folds a folder, and opens or closes a card, thread or folded block on the Overview.

The wheel scrolls the pane under the pointer: the diff or the list beside it. Over the Files changed list, it scrolls the list and leaves the selected file and its diff alone; `j` or `k` brings the list back to the selection. On the Overview, the wheel moves like `j` and `k`.

revu captures the mouse for this, so selecting text needs the terminal's bypass modifier: Shift in most terminals, Option or Fn in some.

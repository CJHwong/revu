# Keys

Every key, by where it works. Press `?` anywhere in revu for the same list.

## Anywhere

| Key | Action |
|---|---|
| `Ctrl+L` (or `Cmd+L`), or paste | Open a pull request by link or `owner/repo#123`. `Cmd+L` works only in terminals that pass Cmd to applications through the kitty keyboard protocol and do not use `Cmd+L` themselves. `Ctrl+L` works everywhere. |
| `?` | Show every key. `j`/`k` and `Space`/`b` scroll it; any other key closes it. |
| `,` | Settings: the theme, the workspace folders, how many days back the inbox looks, and the editor `o` opens files in. `Enter` edits the selected one and saves it to its line in the config at once; comments and other lines stay as they are. Editor lists your shell's editor, the editors revu finds installed, and Custom command. A new number of days reloads the inbox. |
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
| `h`, `q` | Back to the inbox |
| `Esc` | Clear the find; it never leaves the pull request |
| `g` / `G`, `Space` / `b` | Top, bottom, page down, page up |
| `r` | Fetch the pull request again |
| `R` | Review: `Tab` picks Comment, Approve or Request changes. `ctrl+s` asks to confirm and `y` sends. Your pending line comments go with it. Request changes needs a comment; Comment needs a comment or line comments. Only Comment applies on your own PR, as GitHub requires, and on a merged or closed one. |
| `a` | Hand off to a coding agent. A box shows what revu attaches: the PR, its base and head, your clone, and in select lines (`S`) the marked lines and the thread on the cursor's line. Write what you want; `Tab` completes a word from the diff's words and file names. Then `ctrl+s` lists where it goes. See [Hand a spot to a coding agent](how-to.md#hand-a-spot-to-a-coding-agent). |
| `X` | Discard your pending review and every line comment in it, after a `y`. |
| `M` | Merge: the methods the repo allows, as on GitHub, starting on your usual one. `Enter` picks one and `y` sends it. When GitHub blocks the merge, a method turns on auto-merge instead, where the repo allows it; an admin can tick Bypass requirements to merge at once. When auto-merge is on, `M` turns it off. While GitHub is still checking the PR, `r` fetches it again. |
| `o` | Open in the browser. On a file (Files changed, select lines, a full file, a repo match), `o` asks: Editor or GitHub, starting on Editor when your clone has the file. GitHub opens the PR's diff at the file, in select lines at the cursor's line, and a repo match at its line at the PR head. Editor opens the file in your clone at the cursor's line, the match's line, or the file's first change, in `$VISUAL` or `$EDITOR` (else `vi`) or the [`editor`](configuration.md#editor) you set. The first time, revu asks which editor to use and remembers it; Settings (`,`) changes it. A terminal editor takes over until you quit it; VS Code, Zed, Sublime Text and the JetBrains IDEs open the clone beside revu. It opens the clone's working tree, which is the PR's code only when its branch is checked out; the menu says when the clone is on another branch. |
| `d` | Run the `open_diff` command |

## Overview

| Key | Action |
|---|---|
| `j` / `k` | Next or previous comment, review thread or folded block when its top is on screen. Otherwise a third of a screen, so a long comment is read through, not skipped. The cursor never stops on an empty line. |
| `Enter` | Open or close the card, review thread or `<details>` block under the cursor. On the merge box, `Enter` on the Checks row opens the Checks tab, and on the Comments row shows only unresolved threads. |
| `]` / `[` | Next or previous card or folded block |
| `f` / `F` / `n` | Filter the comments (All, Unresolved, Mine), flip oldest or newest first, hide bots. Mine shows only your own words, with the threads you replied in open. |
| `e` | Labels: type to filter, `↑`/`↓` move, `Space` ticks, `Enter` applies the changes to GitHub. |
| `i` | Images: every image and mermaid diagram in the pull request, the description first, then the comments in the list's order. A click on an `[image: ...]` line or a mermaid block, or `Enter` with the cursor on it, opens the gallery at that image. Otherwise `i` starts at the first image on screen. See [Images](#images). |

## Images

The gallery shows one image or diagram at a time, in a box over the pull request. `h` / `l` (or `←` / `→`) go to the previous or next one, `o` opens the current image in the browser, and `Esc` or `q` closes it.

A ` ```mermaid ` block shows as `[diagram: mermaid]` above its source. In the gallery, revu draws it as a picture itself, offline, with no browser or Node.

Ghostty and kitty show the real image through the kitty graphics protocol, and iTerm2 and WezTerm through iTerm2's. revu tells them apart by `TERM` and `TERM_PROGRAM`. Every other terminal, and any terminal inside tmux, shows the image in colored half blocks. revu downloads images with `curl`, and sends your `gh` token only to GitHub's own hosts, so attachments on private repositories load. It shows PNG, JPEG, GIF, WebP and SVG. An SVG cannot load files from your disk.

## Files changed

A file with review threads shows their count, such as `●3`, and the diff pane's title counts them, the outdated ones apart: `4 threads (3 outdated), S to read`. In select lines, each thread sits under its line, folded to its first comment. `t` opens or closes every thread in the file, a click opens or closes one, and `]` / `[` move the cursor from one thread's line to the next; `[` above the first one goes to the outdated threads at the top. `r` replies to the thread on the cursor's line, and `u` resolves it or opens it again; on the first row they reach the outdated thread, and where a line holds several threads, they act on the one you clicked. A reply joins your pending review when you have one, as on GitHub's page, and is posted at once when you don't. Resolved threads are muted, and outdated ones, no longer on a line of the diff, are listed at the top. The title also names the function or class the hunk at the top changes, such as `in BaseApp > on_mount`, and select lines names the one around the cursor. This works for Python, Rust, JavaScript, TypeScript and Go. The diff pane shows it when the view's hunks match git's, as in the word view and delta.

## Go to definition

In select lines, `D` lists the names on the cursor's line, calls first, in a prompt. `Tab` completes one, or type any name, and `Enter` looks it up. A line with one name skips the prompt. revu finds the files at the PR head that hold the name, the changed files first, and opens each definition in the file view: `n`/`N` go to the next and previous, `o` opens it on GitHub or in your editor, and `esc` goes back to select lines. It reads at most 300 files, so a common name like `get` may miss some; the title says so. A name from a library has no definition in the repo.

| Key | Action |
|---|---|
| `Enter` | Show the whole file with its changes marked, drawn by the current renderer, with syntax colors. On a folder, fold or open it. |
| `t` | Switch the list between a folder tree and flat paths |
| `z` | Hide or show the list, so the diff gets the full width |
| `y` | Copy the selected file's path, or the folder's path with a trailing `/`. |
| `v` | Toggle Viewed. It syncs to GitHub's Viewed checkbox at once. On a folder, it marks every file under it viewed, or unmarks them all when every one is viewed already. |
| `V` | Hide viewed files |
| `]` / `[` | Next or previous unviewed file |
| `S` | Select lines in the selected file: its plain diff with line numbers. `j`/`k` or the wheel move, `J`/`K` and `Space`/`b` scroll without moving the cursor, to read a long thread, `V` marks a range (on one side only, as GitHub requires), `Enter` writes a comment, `y` copies the line or range, `D` goes to the definition of a name on the line, `t` opens or closes every review thread in the file, `]` / `[` go to the next or previous line with a thread, `r` replies to it, `u` resolves or reopens it, `o` opens it on GitHub or in your editor, `w` wraps long lines or cuts them, as in the diff, `S` or `esc` goes back. The comment joins your pending review on GitHub, which only you see until `R` submits it; a dot marks lines it already covers. |

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

In select lines (`S`), `y` copies the line under the cursor, or the range `V` marked, as Markdown: the path and line numbers, then the lines in a `diff` block with their `+` and `-` marks. A copied range can span removed and added lines. The numbers are the new file's, or the old file's when every line was removed:

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
- While you type, the prompt offers a word of 4 or more characters from the diff that starts with your text, or a changed file's path, name or folder. Words from the changed lines come before words from the context lines, the most used first. `Tab` takes it.
- Every match is marked, the current one stronger, and the pane title shows `/text  match 2 of 3 in file 5 of 63`.
- On Files changed, `n` past a file's last match goes on to the next file that holds the text, opening its folder. On Commits, it searches the commit on screen.
- On Files changed, `n` past the last file's last match goes on to the matches in every other file in the repo at the PR head. Each opens at its line with the text marked, with syntax colors from `bat` when it is installed. The title shows the line and the block around it: `src/app.rs:110 ... repo match 3 of 155, 5 in the diff  n/N next/previous   in impl App > run`. Past the last one, `n` comes back to the diff; `esc` goes back at once. The footer counts them: `match 1 of 2 in file 1 of 3, 155 in the repo`. git stops at 1000 matches, shown as `1000+`. Binary, untracked and ignored files are not searched.

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

A click works like the keys on what it hits: it opens a pull request in the inbox, switches a tab, selects a file, commit or check, folds a folder, opens or closes a card, thread or folded block on the Overview, opens the gallery at an image, or a review thread in select lines.

The wheel scrolls the pane under the pointer: the diff or the list beside it. Over the Files changed list, it scrolls the list and leaves the selected file and its diff alone; `j` or `k` brings the list back to the selection. On the Overview, the wheel moves like `j` and `k`.

revu captures the mouse for this, so selecting text needs the terminal's bypass modifier: Shift in most terminals, Option or Fn in some.

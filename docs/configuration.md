# Configuration

## Files

| File | What it holds |
|---|---|
| `~/.config/revu/config.yaml` | Your settings. The first run writes it with comments. Press `r` in the inbox to reload it. `$XDG_CONFIG_HOME/revu/config.yaml` when `XDG_CONFIG_HOME` is set. |
| `~/.config/revu/view.yaml` | The view toggles, written whenever you change one, so the next start brings them back. |

A `rs-github-inbox` folder from before the rename to revu moves to `revu` on the first run.

## config.yaml

`groups`, `open_diff` and `diff_renderers` have built-in defaults. The first run writes them into the file commented out, so you can see them. While a key stays commented out, or is missing, revu uses its current default, and a new release can improve it. A key you set replaces its default.

### groups

One GitHub search per inbox group, in display order. Without `groups`, the inbox has five: Needs your review, Needs your teams' review, Your drafts, Waiting for review or checks, and Reviewed by you. `groups: []` leaves the inbox empty.

| Field | Meaning |
|---|---|
| `title` | The group's name in the inbox |
| `query` | A GitHub search. revu adds `is:pr is:open archived:false`, and `sort:updated-desc` unless the query has its own `sort:`. |
| `collapsed` | `true` folds the group on start |

A group shows at most 100 pull requests, the newest ones. Its header says `100 of 140` when it has more.

### updated_within_days

Adds `updated:>=<date>` to every query. Remove the key, or set it to `null`, to see older pull requests. Settings (`,`) changes it too; an empty value there means no limit.

### theme

The color theme. Each one pairs with the `bat` theme of the same name, so code in diffs matches the rest of the screen.

- Dark: `dracula` (the default), `catppuccin_mocha`, `catppuccin_macchiato`, `catppuccin_frappe`, `nord`, `gruvbox_dark`, `solarized_dark`, `one_half_dark`, `monokai`
- Light: `catppuccin_latte`, `gruvbox_light`, `solarized_light`, `one_half_light`, `github`
- `terminal`: your terminal's own colors

Pull request states (open, draft, merged, closed) keep GitHub's colors in every theme.

### workspace_dir

The folder that holds your clones, or a list of folders. The Files changed tab asks for one the first time and saves it here.

```yaml
workspace_dir: ~/Workspace
# or
workspace_dir:
  - ~/Workspace
  - ~/Workspace/side_projects
```

revu searches the folders in order, and a new clone goes into the first. Settings (`,`) edits the list with commas between the folders, and saves it on one line. See [How it works](how-it-works.md#local-clones) for how revu finds a clone.

### diff_renderers

The diff views that `m` switches between. Each runs as a shell command inside the clone.

| Field | Meaning |
|---|---|
| `name` | Shown in the diff pane's title, and saved in `view.yaml` |
| `command` | The shell command. See the placeholders below. |
| `highlight` | `true` adds syntax colors from `bat`, and shows the renderer's red and green as a background tint. It is on for `word` and `difftastic`, which color little of the code themselves. Without `bat`, the output shows as the renderer drew it. |
| `requires` | The program the renderer needs. When it is not on PATH, `m` skips the renderer, and the first diff you open says what is missing. |
| `install` | How to install that program, for the warning |

The placeholders, replaced and quoted for the shell:

| Placeholder | Value |
|---|---|
| `{base}`, `{head}` | The pull request's base and head commits |
| `{paths}` | The file or files to show |
| `{width}` | The diff pane's width |
| `{syntax_theme}` | The theme's `bat` theme |
| `{added_bg}`, `{removed_bg}` | The theme's tints for added and removed lines |

Write `{paths}`, not `"{paths}"`: the values are already quoted.

The defaults are `word` (`git diff --word-diff`), `delta` and `difftastic`. The default `delta` command passes the theme placeholders, so all three share one set of syntax colors and tints, and follow a theme change. A list in your config replaces all three.

When the saved renderer is not installed, revu shows `word` instead and keeps the saved choice for when it is installed. A renderer without `requires` that fails with "command not found" is skipped the same way. When the diff pane is narrower than 100 columns, difftastic draws one column instead of two, unless you set `DFT_DISPLAY` yourself.

### open_diff

The shell command that `d` runs. `{repo}`, `{number}` and `{url}` are replaced. The default shows `gh pr diff {number} -R {repo}` in delta, or in `less` when delta is not installed.

## view.yaml

| Field | Key | Meaning |
|---|---|---|
| `newest_first` | `F` | Comment order |
| `no_bots` | `n` | Hide bot comments |
| `diff_wrap` | `w` | Wrap long diff lines |
| `renderer` | `m` | The diff view, by name |
| `tree_view` | `t` | The file list as a folder tree |
| `hide_viewed` | `V` | Hide viewed files |
| `file_list_hidden` | `z` | Hide the file list |

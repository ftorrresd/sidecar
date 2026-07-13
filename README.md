# sidecar

A minimal side-by-side diff/review TUI for agentic coding workflows. Open it
next to your coding agent to review your working tree.

- Shows the **project-wide diff** by default (`PROJECT-ROOT`).
- Select a file to see **its diff**, or its **contents** when it has no changes.
- Diff-aware search, hunk navigation, `$EDITOR` integration, and mouse support.
- Optional auto-refresh (`R`) tracks your agent's edits live.
- Jumps around with **yazi**, **fzf**, and **ripgrep**.

It doesn't reimplement git/diff tooling — it orchestrates the tools you already
have: `git` for state, `delta` for diffs, `bat` for file contents, `yazi` for
navigation, `ripgrep` + `fzf` for search.

## Install

```sh
sudo pacman -S --needed git git-delta bat yazi ripgrep fzf
cargo install --path .
```

## Usage

```sh
sidecar            # review the repo containing the current directory
sidecar <dir>      # review the repo containing <dir>
```

### Keybindings

Focus moves between the two panels; `j`/`k` act on whichever panel has focus
(shown by a cyan border).

Press `?` at any time for this list in-app.

| Key         | Action                                             |
|-------------|----------------------------------------------------|
| `?`         | Toggle the keybinding help overlay                 |
| `h` / `l`   | Focus the left (files) / right (preview) panel     |
| `j` / `k`   | Focused panel: move file selection, or scroll preview |
| `g` / `G`   | Focused panel: jump to first/last item, or top/bottom |
| `[` / `]`   | Previous / next hunk                                |
| `Enter`     | Open the current hunk's file in sidecar (its diff)  |
| `PgDn`/`PgUp`/`Space` | Page the preview                          |
| `H`         | Jump to PROJECT-ROOT — the project-wide diff       |
| `S`         | Toggle the left panel (hidden ⇒ `h`/`l` do nothing) |
| `W`         | Toggle preview line wrapping                        |
| `1` / `2` / `3` | Diff layout: stacked / side-by-side / auto (default) |
| `e`         | Open the current file/hunk in `$EDITOR`            |
| `y`         | Open **yazi** to pick any file                     |
| `f`         | **fzf** over all filenames (`rg --files`)          |
| `/`         | Search the **project diff** (added/removed lines)  |
| `s`         | Search the **current file's diff**                 |
| `r`         | Refresh now                                        |
| `R`         | Toggle auto-refresh (off by default)               |
| `Ctrl+D`, or `Ctrl+C` `Ctrl+C` | Quit                            |

Selecting a file that has changes shows its diff; an unchanged file shows its
contents; an untracked file shows as a new-file diff.

**Auto** diff layout uses side-by-side when the preview is at least 120 columns
wide, and stacked (unified) below that.

**Search** (`/` and `s`) is diff-aware: it searches only the added/removed lines
of the diff, and jumps to the matching file and line.

**Mouse:** click a file to select it, click a panel to focus it, and scroll the
wheel to move the selection (over the list) or scroll (over the preview).

## Open it beside your agent (tmux)

```tmux
bind-key A split-window -h -l 45% -c "#{pane_current_path}" "sidecar"
```

`prefix + A` opens a review pane next to the current one.

## Note on `test-project/`

`test-project/` is a fixture for trying the tool out. It is now its own git
repository, with a baseline commit representing the original (pre-edit) files,
so its modifications show up as diffs:

```sh
sidecar test-project     # or: cd test-project && sidecar
```

It was set up without altering any working file — a `.git/` was added and the
baseline commit was built from the parent repo's original blobs (`git fetch` +
`git reset`).

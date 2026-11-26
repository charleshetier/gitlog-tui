# gitlog

Git tools for the terminal, built on one shared core:

- `gitlog`: an interactive git graph. Browse the history of every branch, open a commit to see its files, then the diff of a file.
- `gitchange`: the changes that are not committed yet. It opens straight on the list of changed files, then the diff of a file, then the whole file: the same views as `gitlog`, without the graph.
- `gitmerge`: the conflicted files of a merge, a rebase or a cherry-pick, one at a time, on three columns: ours on the left, theirs on the right, and in the middle the result, which you edit.

## Install

You need [Rust](https://rustup.rs) 1.88 or newer and `git` on your `PATH`.

Straight from GitHub:

```sh
cargo install --git https://github.com/charleshetier/gitlog-tui
```

Or from a local clone:

```sh
git clone https://github.com/charleshetier/gitlog-tui
cd gitlog-tui
cargo install --path .
```

Both install the `gitlog`, `gitchange` and `gitmerge` commands into `~/.cargo/bin`. Run the same command again to update.

## Use

```sh
gitlog            # in the current repository
gitlog path/to/repo
gitchange        # the uncommitted changes of the current repository
gitchange path/to/repo
gitmerge         # the conflicted files of the current repository
gitmerge path/to/repo
```

Press `?` inside any of the tools for the list of shortcuts, or run it with `--help`.

`gitchange` compares the working tree and the index to HEAD, and lists the untracked files too. `Enter` opens the diff of a file, `Enter` again the whole file; `R` reads the changes again. The selection with the mouse, the search with `/` and the wrapping with `w` work as in `gitlog`; what belongs to the graph (commit search, branch commands, `h`) is not there.

Drag the mouse over the code of a diff to select it: the text is copied to the clipboard when the button is released.

Press `h` in the graph to jump to the HEAD commit, without opening it.

Press `w` to wrap the long lines instead of cutting them, in the graph and in the code of a diff; press it again to cut them.

Press `b` for the branch commands: `b` `f` opens the list of branches, to pick with `Space` the ones the graph shows (`v` checks all or none, `/` searches the list, `Enter` applies); `b` `o` shows or hides the remote branches, `b` `O` shows the remote branches only; `b` `h` opens the branch hierarchy.

Press `/` on a diff to search its code. The matches follow the typing; `Enter` keeps the search, then `n` and `N` go to the next and previous match, and `Esc` clears it. The case is ignored unless the search has a capital letter.

## gitmerge

Run `gitmerge` when a merge, a rebase or a cherry-pick stops on conflicts. It shows one conflicted file at a time; `n` and `N` go to the next and previous one.

The three columns are aligned block by block. A change made by one side only is already in the result, as git would take it; a conflict waits, with the common ancestor in the result, until you settle it:

- `>` takes ours, `<` takes theirs. In a conflict, taking the other side afterwards adds its lines below the first; taking a side twice keeps that side alone. The same keys work on any changed block, to put a change back or leave it out. The arrows `»` and `«` beside a block do the same with a click.
- `x` keeps the result as it is, and the conflict is settled.
- `]` and `[` jump to the next and previous conflict, `}` and `{` to the next and previous change. `Tab` and `Shift+Tab`, `c` and `C` do the same, for keyboards where brackets and braces are far away.

The result is edited as in vi. The keys are commands in normal mode; `i`, `a`, `I`, `A`, `o`, `O` or `Enter` switch to insert mode, where they are typed, and `Esc` comes back. `dd` deletes a line, `u` undoes and `Ctrl+R` redoes, block choices included.

`Ctrl+S` writes the result to the file and nothing more. `S` writes it and stages the file (`git add`), which tells git its conflict is resolved, then opens the next file that still waits; it refuses while a conflict of the file is not settled. When every file is staged, the tool says how to conclude: `git commit` for a merge, `git rebase --continue` for a rebase. Quitting asks first when a result is not written.

`/` searches the result, `w` wraps the long lines, `y` copies the line under the cursor and `R` reads the conflicts again from git, as in the other tools.

What it does not do yet: a file deleted on one side, a binary file and a file that is not UTF-8 are listed but have to be settled with git; the result always starts from the versions git keeps in its index, so hand edits already made in the conflicted file are not picked up (a message says so); and there is no selection with the mouse in this tool.

## Display

The icons expect a [Nerd Font](https://www.nerdfonts.com). Without one, use `--plain` or set `GITLOG_PLAIN=1`.

Code is syntax highlighted on terminals with 24-bit colours: always on Windows, elsewhere when `COLORTERM` is `truecolor` or `24bit`. Use `--no-syntax` to turn it off.

The branch hierarchy view (`b` `h`) needs git 2.41 or newer.

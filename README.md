# gitlog

Git tools for the terminal, built on one shared core:

- `gitlog`: an interactive git graph. Browse the history of every branch, open a commit to see its files, then the diff of a file.
- `gitchanges`: the changes that are not committed yet. It opens straight on the list of changed files, then the diff of a file, then the whole file: the same views as `gitlog`, without the graph.

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

Both install the `gitlog` and `gitchanges` commands into `~/.cargo/bin`. Run the same command again to update.

## Use

```sh
gitlog            # in the current repository
gitlog path/to/repo
gitchanges        # the uncommitted changes of the current repository
gitchanges path/to/repo
```

Press `?` inside either tool for the list of shortcuts, or run it with `--help`.

`gitchanges` compares the working tree and the index to HEAD, and lists the untracked files too. `Enter` opens the diff of a file, `Enter` again the whole file; `R` reads the changes again. The selection with the mouse, the search with `/` and the wrapping with `w` work as in `gitlog`; what belongs to the graph (commit search, branch commands, `h`) is not there.

Drag the mouse over the code of a diff to select it: the text is copied to the clipboard when the button is released.

Press `h` in the graph to jump to the HEAD commit, without opening it.

Press `w` to wrap the long lines instead of cutting them, in the graph and in the code of a diff; press it again to cut them.

Press `b` for the branch commands: `b` `f` opens the list of branches, to pick with `Space` the ones the graph shows (`v` checks all or none, `/` searches the list, `Enter` applies); `b` `o` shows or hides the remote branches, `b` `O` shows the remote branches only; `b` `h` opens the branch hierarchy.

Press `/` on a diff to search its code. The matches follow the typing; `Enter` keeps the search, then `n` and `N` go to the next and previous match, and `Esc` clears it. The case is ignored unless the search has a capital letter.

The icons expect a [Nerd Font](https://www.nerdfonts.com). Without one, use `--plain` or set `GITLOG_PLAIN=1`.

Diffs are syntax highlighted on terminals with 24-bit colours: always on Windows, elsewhere when `COLORTERM` is `truecolor` or `24bit`. Use `--no-syntax` to turn it off.

The branch hierarchy view (`b` `h`) needs git 2.41 or newer.

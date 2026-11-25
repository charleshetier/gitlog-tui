# gitlog

An interactive git graph for the terminal: browse the history of every branch, open a commit to see its files, then the diff of a file.

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

Both install the `gitlog` command into `~/.cargo/bin`. Run the same command again to update.

## Use

```sh
gitlog            # in the current repository
gitlog path/to/repo
```

Press `?` inside `gitlog` for the list of shortcuts, or run `gitlog --help`.

Drag the mouse over the code of a diff to select it: the text is copied to the clipboard when the button is released.

The icons expect a [Nerd Font](https://www.nerdfonts.com). Without one, use `gitlog --plain` or set `GITLOG_PLAIN=1`.

Diffs are syntax highlighted on terminals with 24-bit colours: always on Windows, elsewhere when `COLORTERM` is `truecolor` or `24bit`. Use `gitlog --no-syntax` to turn it off.

The branch hierarchy view (`h`) needs git 2.41 or newer.

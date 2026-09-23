# Markd Smith

A minimal GTK4 / libadwaita Markdown editor for Linux: a split pane with a
syntax-highlighted [GtkSourceView](https://wiki.gnome.org/Projects/GtkSourceView)
editor on the left and a live [WebKitGTK](https://webkitgtk.org/) preview on the
right. Every keystroke re-parses the document with
[pulldown-cmark](https://github.com/raphlinus/pulldown-cmark) and reloads the
preview.

It is also a learning project: `src/main.rs` is deliberately a single file with
`// LEARN:` comments explaining the GTK4/Rust concepts as they appear.

## Features

- Live side-by-side Markdown preview (`gtk4::Paned`, draggable split)
- Mermaid diagrams — ` ```mermaid ` fences render as diagrams; the ~3 MB mermaid
  bundle is embedded in the binary but only injected into documents that use it
- Light/dark theme following libadwaita, with a matching mermaid theme
- Open/save with a save-on-quit dialog, keyboard shortcuts for pane focus and
  pane swapping

## System dependencies

The Rust crates are FFI wrappers, so the C libraries must be present at both
build time and run time. On Debian/Ubuntu:

```bash
sudo apt install -y libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev libwebkitgtk-6.0-dev
```

Fedora:

```bash
sudo dnf install -y gtk4-devel libadwaita-devel gtksourceview5-devel webkitgtk6.0-devel
```

Arch:

```bash
sudo pacman -S --needed gtk4 libadwaita gtksourceview5 webkitgtk-6.0
```

You also need a Rust toolchain (edition 2021; developed against Rust 1.96).
Install with [rustup](https://rustup.rs) if you don't have one.

> The first build takes roughly 2–5 minutes because the WebKit and GTK bindings
> are large. Subsequent builds are incremental and fast.

## Ways to build

### 1. Run straight from the source tree (day-to-day development)

```bash
cargo run                      # open the built-in welcome document
cargo run -- path/to/file.md   # open a specific file
```

This is the recommended way to work on the app: `.cargo/config.toml` sets
`WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` for you, which keeps the WebKit
subprocess from requiring a sandboxed environment on a normal dev machine.

### 2. Debug build

```bash
cargo build
./target/debug/markd_smith path/to/file.md
```

Fast to compile, slow to run, and includes debug symbols — use it with `gdb` or
`rust-gdb`.

### 3. Release build

```bash
cargo build --release
./target/release/markd_smith path/to/file.md
```

Optimized and much faster at rendering large documents. This is the binary to
copy around if you want to move it to another machine with the same GTK stack.

### 4. Install into `~/.cargo/bin`

```bash
cargo install --path .
```

This does a release build and drops the `markd_smith` binary into
`~/.cargo/bin`, so it is on your `PATH` (assuming rustup added that directory to
your shell profile). See [Replacing an installed binary](#replacing-an-installed-binary)
for how to update it later.

The binary is self-contained as far as *assets* go — the mermaid bundle and CSS
are compiled in with `include_str!` — but it still links dynamically against the
system GTK/WebKit libraries, so those packages must stay installed.

### 5. Desktop integration (menu entry and icon)

`data/` contains a freedesktop `.desktop` entry and hicolor icons for the app ID
`io.github.underhilllabs.MarkdSmith`. After `cargo install --path .`:

```bash
install -Dm644 data/io.github.underhilllabs.MarkdSmith.desktop \
  ~/.local/share/applications/io.github.underhilllabs.MarkdSmith.desktop
cp -r data/icons/hicolor ~/.local/share/icons/
update-desktop-database ~/.local/share/applications
gtk4-update-icon-cache -f -t ~/.local/share/icons/hicolor
```

The `.desktop` file's `Exec=markd_smith %F` resolves via `PATH`; if your desktop
session doesn't include `~/.cargo/bin` in its `PATH`, change `Exec` to the
absolute path (`/home/<you>/.cargo/bin/markd_smith %F`).

> **Sandbox note:** the `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` variable in
> `.cargo/config.toml` only applies to processes Cargo launches (`cargo run`).
> An installed binary launched from your app menu does not get it. If the
> preview pane comes up blank when launched that way, add the variable to the
> desktop entry's `Exec` line:
> `Exec=env WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 markd_smith %F`

## Replacing an installed binary

`cargo install` refuses to overwrite an existing install of the same package at
the same version, so pass `--force` (`-f`):

```bash
cd /path/to/markdown_smith
cargo install --path . --force
```

That rebuilds in release mode and atomically replaces `~/.cargo/bin/markd_smith`.
Useful variations:

```bash
cargo install --path . --force --locked   # build with the exact Cargo.lock versions
cargo uninstall markd_smith               # remove it entirely (uses the *package* name)
cargo install --list                      # show what's installed and which binaries
```

If you'd rather not go through Cargo — for example you already have a release
build you tested — copy it in place directly:

```bash
cargo build --release
install -m755 target/release/markd_smith ~/.cargo/bin/markd_smith
```

Use `install` rather than `cp` when the app may be running: `install` replaces
the file by rename, whereas overwriting a busy executable in place can fail with
"Text file busy".

## Development

```bash
cargo fmt                    # format
cargo clippy -- -D warnings  # lint; warnings are errors
cargo test                   # unit tests (markdown → HTML conversion)
```

Tests live inline in `src/main.rs` under `#[cfg(test)]` and are named after the
behavior they check, e.g. `renders_mermaid_fences_as_diagram_blocks`.

## Architecture

```
adw::ApplicationWindow
  └── adw::ToolbarView
        ├── adw::HeaderBar
        └── gtk4::Paned (horizontal split)
              ├── ScrolledWindow → sourceview5::View   (editor)
              └── webkit6::WebView                     (preview)
```

Data flow: `sourceview5::Buffer` → `connect_changed` → `markdown_to_html()`
(pure Rust) → `webkit6::WebView::load_html()`.

Note that this uses the `webkit6` crate, not `webkit2gtk` — the latter links
against GTK3, while `webkit6` targets `libwebkitgtk-6.0` for GTK4.

See `CLAUDE.md` and `AGENTS.md` for further contributor notes.

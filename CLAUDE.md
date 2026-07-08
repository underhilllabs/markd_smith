# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```bash
cargo run                        # launch editor with welcome document
cargo run -- path/to/file.md    # open a specific file
cargo build                      # debug build
cargo build --release            # optimized build
cargo fmt                        # format source
cargo clippy -- -D warnings      # lint (warnings are errors)
cargo test                       # run tests
```

First build is slow (2–5 min) because WebKit is large. Subsequent builds are incremental.

## System Dependencies

The Rust crates are FFI wrappers; these C libraries must be installed:

```bash
sudo apt install -y libgtk-4-dev libadwaita-1-dev libgtksourceview-5-dev libwebkitgtk-6.0-dev
```

`.cargo/config.toml` sets `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` for local dev so the WebKit subprocess doesn't require a sandboxed environment.

## Architecture

Single binary, single file (`src/main.rs`). The file is intentionally kept as one file — this is a learning project with embedded `// LEARN:` comments explaining GTK4/Rust concepts.

**Data flow:** `sourceview5::Buffer` → `connect_changed` signal → `markdown_to_html()` (pure Rust, pulldown-cmark) → `webkit6::WebView::load_html()`

**Widget tree:**
```
adw::ApplicationWindow
  └── adw::ToolbarView
        ├── adw::HeaderBar (top bar)
        └── gtk4::Paned (horizontal split, starts at 600px)
              ├── ScrolledWindow → sourceview5::View (editor, left)
              └── webkit6::WebView (preview, right)
```

**Key GTK4 idioms used here:**
- `use adw::prelude::*` re-exports all of `gtk4::prelude`, so only two prelude imports are needed
- GObject clones are reference-count bumps — cheap; used to move `web_view` into the signal closure
- `adw::ToolbarView` is required (not optional) for the flat header style to render without glitches
- `app.run_with_args::<&str>(&[])` passes an empty arg list to GTK so it doesn't try to open our filename via GIO

**Why `webkit6` not `webkit2gtk`:** `webkit2gtk` links against GTK3; `webkit6` links against `libwebkitgtk-6.0-dev` (the GTK4 variant).

## Testing

No tests yet. Add unit tests for `markdown_to_html` as `#[cfg(test)]` inline tests (it's a pure function — easy to test). Name tests after behavior, e.g. `renders_fenced_code_blocks`.

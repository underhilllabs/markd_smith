# markdown_smith: GTK4 Markdown Editor — Implementation Plan

## Context

Build a brand-new Rust desktop application for GNOME/Ubuntu 24.04 that is a split-pane markdown editor: raw markdown on the left (with syntax highlighting), live HTML preview on the right. The user is new to Rust and GTK4, so this is explicitly a learning project — all code lives in a single `src/main.rs` with explanatory comments, no premature abstractions.

**Tech stack (user-specified):**
- `gtk4-rs` + `libadwaita` — UI framework
- `sourceview5` — syntax-highlighted editor widget
- `pulldown-cmark` — markdown → HTML parser
- `webkit6` — GTK4-native WebKit bindings for rendering the HTML preview

---

## Step 0: System Dependencies

Install the C library dev packages (Rust crates are thin FFI wrappers that call pkg-config at build time):

```bash
sudo apt install -y \
  libgtk-4-dev \
  libadwaita-1-dev \
  libgtksourceview-5-dev \
  libwebkitgtk-6.0-dev \
  build-essential \
  pkg-config
```

Verify:
```bash
pkg-config --modversion gtk4              # expect 4.14.x
pkg-config --modversion libadwaita-1      # expect 1.5.x
pkg-config --modversion gtksourceview-5   # expect 5.12.x
pkg-config --modversion webkitgtk-6.0     # expect 2.52.x
```

---

## Step 1: Initialize the Project

```bash
cd /home/bart/projects/markdown_smith
cargo init
```

---

## Step 2: Cargo.toml

Replace generated `Cargo.toml` with:

```toml
[package]
name = "markdown_smith"
version = "0.1.0"
edition = "2021"

[dependencies]
gtk4 = { version = "0.9", features = ["v4_14"] }
adw = { package = "libadwaita", version = "0.7", features = ["v1_5"] }
sourceview5 = { version = "0.10", features = ["v5_12"] }
webkit6 = { version = "0.6", features = ["v2_42"] }
pulldown-cmark = "0.13"
```

**Key decisions:**
- `webkit6` (not `webkit2gtk`) — `webkit2gtk` is GTK3; `webkit6` links against `libwebkitgtk-6.0-dev` which is the GTK4 variant.
- `adw` is the Cargo alias for the `libadwaita` package (conventional shorthand).
- `pulldown-cmark` is pure Rust — no system library needed.

---

## Step 3: src/main.rs

Replace generated `src/main.rs` with the full implementation below. The file has four sections:

### 3a. Imports

```rust
use adw::prelude::*;
use gtk4::prelude::*;
use sourceview5::prelude::*;
use adw::Application;
use gtk4::{Orientation, PolicyType, ScrolledWindow, WrapMode};
use sourceview5::{Buffer, LanguageManager, StyleSchemeManager, View};
use webkit6::WebView;
use webkit6::prelude::WebViewExt;
use pulldown_cmark::{html, Options, Parser};
```

### 3b. `fn markdown_to_html(markdown: &str) -> String`

Pure function — no GTK. Uses `pulldown_cmark::Parser::new_ext` with `Options::all()`, drains events into a string via `html::push_html`, then wraps it in a minimal `<!DOCTYPE html>` document with basic CSS (sans-serif font, code blocks styled, blockquotes, tables).

### 3c. `fn build_ui(app: &Application)`

Sequence:
1. **HeaderBar**: `adw::HeaderBar::new()`
2. **Source buffer**: `sourceview5::Buffer::new(None)`, then get markdown language via `LanguageManager::default().language("markdown")` and `buffer.set_language(Some(&lang))`. Also apply the `"kate"` style scheme from `StyleSchemeManager::default()`.
3. **Editor view**: `sourceview5::View::with_buffer(&source_buffer)` with `set_show_line_numbers(true)`, `set_highlight_current_line(true)`, `set_monospace(true)`, `set_wrap_mode(WrapMode::Word)`.
4. **Editor scroll**: `ScrolledWindow` wrapping the view, `set_vexpand(true)` / `set_hexpand(true)`.
5. **WebView**: `webkit6::WebView::new()`, `set_vexpand(true)` / `set_hexpand(true)`, load initial placeholder HTML via `web_view.load_html(&initial_html, None)`.
6. **Paned**: `gtk4::Paned::new(Orientation::Horizontal)`, `set_start_child(Some(&editor_scroll))`, `set_end_child(Some(&web_view))`, `set_position(600)`, `set_wide_handle(true)`.
7. **ToolbarView**: `adw::ToolbarView::new()`, `add_top_bar(&header_bar)`, `set_content(Some(&paned))`.
8. **Window**: `adw::ApplicationWindow::new(app)`, `set_title(Some("markdown_smith"))`, `set_default_size(1200, 800)`, `set_content(Some(&toolbar_view))`.
9. **Signal**: `let web_view_clone = web_view.clone(); source_buffer.connect_changed(move |buffer| { … })` — inside the closure: extract text with `buffer.text(&start, &end, false)`, call `markdown_to_html()`, call `web_view_clone.load_html(&html, None)`.
10. `window.present()`

### 3d. `fn main()`

```rust
fn main() {
    let app = Application::new(
        Some("com.example.markdown-smith"),
        gtk4::gio::ApplicationFlags::empty(),
    );
    app.connect_activate(build_ui);
    let exit_code = app.run();
    std::process::exit(exit_code.into());
}
```

---

## Step 4: Build and Run

```bash
cargo build          # first build takes 2–5 min (WebKit is large)
cargo run
```

---

## Verification

1. Window appears at 1200×800 with header bar and 50/50 split pane.
2. Left pane shows syntax-highlighted placeholder text (if `markdown.lang` is installed at `/usr/share/gtksourceview-5/language-specs/`).
3. Type `# Hello` in the editor → right pane immediately renders a large heading.
4. `**bold**`, `*italic*`, `` `code` ``, lists, and fenced code blocks all render correctly.
5. Dragging the pane divider resizes both sides fluidly.

---

## Known Pitfalls

| Symptom | Fix |
|---|---|
| `pkg-config not found` during build | `sudo apt install pkg-config` |
| `Could not find webkitgtk-6.0` | `sudo apt install libwebkitgtk-6.0-dev` |
| No syntax highlighting on left | `sudo apt install libgtksourceview-5-common` |
| `webkit6` version mismatch | Check `cargo search webkit6` for latest version |

---

## Critical Files

- `Cargo.toml` — all dependency versions; wrong here breaks everything
- `src/main.rs` — entire app in one file (intentional for learning)

## Learning Points Embedded in Comments

- `prelude::*` imports — why GTK uses extension traits
- `GObject` reference counting — why `.clone()` is cheap
- Signal system — GTK's Observer pattern via `connect_changed`
- `adw::Application` vs `gtk4::Application` — why Adwaita variant is preferred
- `ToolbarView` pattern — modern libadwaita window structure
- `webkit6` vs `webkit2gtk` — GTK4 vs GTK3 WebKit bindings

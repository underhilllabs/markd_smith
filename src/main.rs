// markdown_smith — a minimal GTK4/libadwaita markdown editor
//
// Architecture: single split pane — sourceview5 editor on the left, webkit6
// preview on the right. Every keystroke re-parses the markdown with
// pulldown-cmark and reloads the WebView.
//
// LEARN notes throughout explain the GTK4/Rust concepts as you encounter them.

// ─────────────────────────────────────────────────────────────────────────────
// IMPORTS
// ─────────────────────────────────────────────────────────────────────────────

// LEARN: "use X::prelude::*" is the gtk-rs idiom for importing trait methods.
// GTK4 Rust bindings put most widget methods behind traits (WidgetExt,
// TextBufferExt, etc.). Without the prelude you can construct widgets but
// cannot call any methods on them.
//
// adw::prelude already re-exports all of gtk4::prelude, so we only need
// two prelude imports here (adw + sourceview5). In a gtk4-only app you
// would write `use gtk4::prelude::*` instead.
use adw::prelude::*;
use sourceview5::prelude::*;

// LEARN: "adw" is the Cargo alias we gave libadwaita in Cargo.toml
// (package = "libadwaita"). Everything GNOME-HIG-specific lives here.
use adw::Application;

// LEARN: We only import the gtk4 types we reference by name. Everything else
// comes in through the prelude traits above.
use gtk4::{Orientation, PolicyType, ScrolledWindow, WrapMode};

// LEARN: sourceview5 provides the syntax-highlighted editor widget.
// It is a superset of gtk4::TextView — every method on TextView also works
// on sourceview5::View because View "IsA<TextView>" in GObject terms.
use sourceview5::{Buffer, LanguageManager, StyleSchemeManager, View};

// LEARN: webkit6 provides an embedded browser widget. We render
// markdown → HTML string and load it here. WebView "IsA<gtk4::Widget>"
// so it can live in any container.
use webkit6::WebView;

// LEARN: WebViewExt is the extension trait that provides load_html(), etc.
// Without this import the WebView struct would exist but have no methods.
use webkit6::prelude::WebViewExt;

// LEARN: pulldown-cmark is a pure-Rust markdown parser. Parser is a lazy
// iterator yielding Events (Start, End, Text, Code, …). html::push_html
// collects those events and writes HTML into a String.
use pulldown_cmark::{html, Options, Parser};

use gtk4::gio;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

const DEFAULT_SHORTCUTS: &[(&str, &str)] = &[
    ("win.open", "<Control>o"),
    ("win.save", "<Control>s"),
    ("win.quit", "<Control>q"),
    ("win.undo", "<Control>z"),
    ("win.redo", "<Control><Shift>z"),
    ("win.editor-only", "<Control>1"),
    ("win.preview-only", "<Control>2"),
    ("win.split-view", "<Control>0"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneMode {
    Split,
    EditorOnly,
    PreviewOnly,
}

// ─────────────────────────────────────────────────────────────────────────────
// MARKDOWN → HTML CONVERSION
// ─────────────────────────────────────────────────────────────────────────────

// LEARN: This function is pure — it has nothing to do with GTK. It takes a
// &str (a borrowed string slice) and returns an owned String. It is called
// from inside a GTK signal handler but is independently testable.
fn markdown_to_html(markdown: &str) -> String {
    // LEARN: Options is a bitflag set. Options::all() enables every extension
    // (tables, footnotes, strikethrough, task lists, smart punctuation…).
    // Use Options::empty() for strict CommonMark only.
    let options = Options::all();

    // LEARN: Parser::new_ext returns a lazy iterator over markdown events.
    // Nothing is parsed until you consume it (via html::push_html below).
    let parser = Parser::new_ext(markdown, options);

    // LEARN: html::push_html drains the parser iterator and appends HTML to
    // the provided String. After this call, html_output holds the fragment.
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);

    // Wrap in a minimal HTML document so WebKit gets correct UTF-8 and
    // sensible default typography. The double braces {{ }} are how you write
    // a literal { } inside a Rust format!() string.
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8">
  <style>
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      font-size: 15px;
      line-height: 1.65;
      max-width: 820px;
      margin: 0 auto;
      padding: 1.25rem 2rem;
      color: #1c1c1e;
    }}
    h1, h2, h3, h4, h5, h6 {{
      margin-top: 1.5em;
      margin-bottom: 0.4em;
      line-height: 1.25;
    }}
    pre {{
      background: #f5f5f5;
      padding: 0.9em 1em;
      border-radius: 6px;
      overflow-x: auto;
    }}
    code {{
      font-family: "JetBrains Mono", "Fira Code", monospace;
      font-size: 0.875em;
      background: #f0f0f0;
      padding: 0.15em 0.35em;
      border-radius: 3px;
    }}
    pre code {{ background: none; padding: 0; }}
    blockquote {{
      border-left: 4px solid #d0d0d0;
      margin: 0;
      padding-left: 1.1em;
      color: #555;
    }}
    table {{ border-collapse: collapse; width: 100%; margin: 1em 0; }}
    th, td {{ border: 1px solid #ddd; padding: 0.45em 0.75em; }}
    th {{ background: #f5f5f5; font-weight: 600; }}
    a {{ color: #0062cc; }}
    img {{ max-width: 100%; }}
  </style>
</head>
<body>
{html_output}
</body>
</html>"#
    )
}

fn shortcut_config_path() -> Option<PathBuf> {
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(config_home).join("markdown_smith/shortcuts.conf"));
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/markdown_smith/shortcuts.conf"))
}

fn default_shortcut_config() -> String {
    let mut config = String::from("# GTK accelerator syntax\n");
    for (action, accel) in DEFAULT_SHORTCUTS {
        config.push_str(action);
        config.push('=');
        config.push_str(accel);
        config.push('\n');
    }
    config
}

fn is_valid_accelerator(accel: &str) -> bool {
    match gtk4::accelerator_parse(accel) {
        Some((key, modifiers)) => gtk4::accelerator_valid(key, modifiers),
        None => false,
    }
}

fn parse_shortcut_config_with_validator(
    contents: &str,
    known_actions: &HashSet<&'static str>,
    is_valid: impl Fn(&str) -> bool,
) -> HashMap<&'static str, String> {
    let mut shortcuts = HashMap::new();

    for (line_number, raw_line) in contents.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((action, accel)) = line.split_once('=') else {
            eprintln!(
                "Ignoring shortcut config line {}: expected action=accelerator",
                line_number + 1
            );
            continue;
        };

        let action = action.trim();
        let accel = accel.trim();
        let Some(&known_action) = known_actions.get(action) else {
            eprintln!(
                "Ignoring shortcut config line {}: unknown action `{action}`",
                line_number + 1
            );
            continue;
        };

        if !is_valid(accel) {
            eprintln!(
                "Ignoring shortcut config line {}: invalid accelerator `{accel}`",
                line_number + 1
            );
            continue;
        }

        shortcuts.insert(known_action, accel.to_string());
    }

    shortcuts
}

fn parse_shortcut_config(
    contents: &str,
    known_actions: &HashSet<&'static str>,
) -> HashMap<&'static str, String> {
    parse_shortcut_config_with_validator(contents, known_actions, is_valid_accelerator)
}

fn load_shortcuts() -> HashMap<&'static str, String> {
    let mut shortcuts = DEFAULT_SHORTCUTS
        .iter()
        .map(|(action, accel)| (*action, (*accel).to_string()))
        .collect::<HashMap<_, _>>();

    let Some(path) = shortcut_config_path() else {
        return shortcuts;
    };

    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!("Could not create shortcut config directory: {err}");
                return shortcuts;
            }
        }

        if let Err(err) = std::fs::write(&path, default_shortcut_config()) {
            eprintln!("Could not write default shortcut config: {err}");
        }
        return shortcuts;
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let known_actions = DEFAULT_SHORTCUTS
                .iter()
                .map(|(action, _)| *action)
                .collect::<HashSet<_>>();
            shortcuts.extend(parse_shortcut_config(&contents, &known_actions));
        }
        Err(err) => eprintln!("Could not read shortcut config: {err}"),
    }

    shortcuts
}

fn apply_configured_shortcuts(app: &Application) {
    let shortcuts = load_shortcuts();

    for (action, _) in DEFAULT_SHORTCUTS {
        if let Some(accel) = shortcuts.get(action) {
            app.set_accels_for_action(action, &[accel.as_str()]);
        }
    }
}

fn write_buffer_to_path(
    buffer: &Buffer,
    window: &adw::ApplicationWindow,
    current_file: &Rc<RefCell<Option<String>>>,
    path: PathBuf,
) -> bool {
    let (start, end) = buffer.bounds();
    let text = buffer.text(&start, &end, false);

    if let Err(err) = std::fs::write(&path, text.as_str()) {
        eprintln!("Save error: {err}");
        return false;
    }

    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    window.set_title(Some(&format!("markdown_smith — {name}")));
    *current_file.borrow_mut() = Some(path.to_string_lossy().into_owned());
    buffer.set_modified(false);
    true
}

fn save_buffer_or_prompt_for_path(
    buffer: Buffer,
    window: adw::ApplicationWindow,
    current_file: Rc<RefCell<Option<String>>>,
    after_save: impl FnOnce() + 'static,
) {
    let existing_path = current_file.borrow().clone();
    if let Some(path) = existing_path {
        if write_buffer_to_path(&buffer, &window, &current_file, PathBuf::from(path)) {
            after_save();
        }
        return;
    }

    let dialog = gtk4::FileDialog::new();
    let parent = window.clone();
    dialog.save(Some(&parent), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result {
            if let Some(path) = file.path() {
                if write_buffer_to_path(&buffer, &window, &current_file, path) {
                    after_save();
                }
            }
        }
    });
}

fn request_quit(
    buffer: Buffer,
    window: adw::ApplicationWindow,
    current_file: Rc<RefCell<Option<String>>>,
    allow_close: Rc<RefCell<bool>>,
) {
    if !buffer.is_modified() {
        *allow_close.borrow_mut() = true;
        window.close();
        return;
    }

    let dialog = gtk4::AlertDialog::builder()
        .modal(true)
        .message("Save changes before quitting?")
        .detail("The current document has unsaved changes.")
        .buttons(["Cancel", "Discard", "Save"])
        .cancel_button(0)
        .default_button(2)
        .build();

    let parent = window.clone();
    dialog.choose(
        Some(&parent),
        gio::Cancellable::NONE,
        move |result| match result {
            Ok(2) => {
                let win = window.clone();
                let close_flag = allow_close.clone();
                save_buffer_or_prompt_for_path(buffer, window, current_file, move || {
                    *close_flag.borrow_mut() = true;
                    win.close();
                });
            }
            Ok(1) => {
                *allow_close.borrow_mut() = true;
                window.close();
            }
            Ok(0) | Err(_) => {}
            Ok(other) => eprintln!("Unexpected quit dialog response: {other}"),
        },
    );
}

fn set_pane_mode(
    requested_mode: PaneMode,
    pane_mode: &Rc<RefCell<PaneMode>>,
    last_split_position: &Rc<RefCell<i32>>,
    paned: &gtk4::Paned,
    editor_scroll: &ScrolledWindow,
    web_view: &WebView,
) {
    let current_mode = *pane_mode.borrow();
    let next_mode = if requested_mode == current_mode {
        PaneMode::Split
    } else {
        requested_mode
    };

    if current_mode == PaneMode::Split && next_mode != PaneMode::Split {
        *last_split_position.borrow_mut() = paned.position();
    }

    match next_mode {
        PaneMode::Split => {
            editor_scroll.set_visible(true);
            web_view.set_visible(true);
            paned.set_position(*last_split_position.borrow());
        }
        PaneMode::EditorOnly => {
            editor_scroll.set_visible(true);
            web_view.set_visible(false);
        }
        PaneMode::PreviewOnly => {
            editor_scroll.set_visible(false);
            web_view.set_visible(true);
        }
    }

    *pane_mode.borrow_mut() = next_mode;
}

// ─────────────────────────────────────────────────────────────────────────────
// UI CONSTRUCTION
// ─────────────────────────────────────────────────────────────────────────────

// LEARN: GTK4 apps separate "create the Application object" (main) from
// "build the window" (this function). The "activate" signal fires when the
// application is ready to show its first window. Multiple activations can
// happen (e.g. a second launch requests focus), so keep this stateless.
fn build_ui(app: &Application, file_path: Option<&str>) {
    // ── Header bar ──────────────────────────────────────────────────────────

    // LEARN: adw::HeaderBar is the libadwaita-aware replacement for
    // gtk4::HeaderBar. It respects the platform's window button positions
    // and integrates with the Adwaita style engine for the "flat" look.
    let header_bar = adw::HeaderBar::new();

    // ── Source editor (left pane) ───────────────────────────────────────────

    // LEARN: sourceview5::Buffer is a GtkTextBuffer subclass that understands
    // programming languages and colour schemes. We create it first because we
    // need the handle to connect the "changed" signal later.
    let source_buffer = Buffer::new(None);

    // LEARN: LanguageManager::default() returns a process-wide singleton that
    // scans the system for .lang files (typically at
    // /usr/share/gtksourceview-5/language-specs/). language("markdown") looks
    // up the Markdown grammar. The if-let silently skips if not found.
    let lang_manager = LanguageManager::default();
    if let Some(markdown_lang) = lang_manager.language("markdown") {
        source_buffer.set_language(Some(&markdown_lang));
        source_buffer.set_highlight_syntax(true);
    }

    // LEARN: StyleSchemeManager is another singleton. "kate" is a light
    // scheme that ships with GtkSourceView. Other common ones: "classic",
    // "cobalt" (dark), "solarized-dark".
    let scheme_manager = StyleSchemeManager::default();
    if let Some(scheme) = scheme_manager.scheme("kate") {
        source_buffer.set_style_scheme(Some(&scheme));
    }

    // LEARN: View::with_buffer() creates the editor widget pre-wired to our
    // buffer. Alternatively: View::new() then view.set_buffer(Some(&buf)).
    let source_view = View::with_buffer(&source_buffer);

    // ViewExt methods (from sourceview5):
    source_view.set_show_line_numbers(true);
    source_view.set_highlight_current_line(true);
    source_view.set_auto_indent(true);
    source_view.set_tab_width(4);

    // LEARN: set_monospace and set_wrap_mode come from TextViewExt (gtk4).
    // sourceview5::View inherits these because it "IsA<gtk4::TextView>".
    source_view.set_monospace(true);
    source_view.set_wrap_mode(WrapMode::Word);

    // LEARN: set_vexpand / set_hexpand tell the GTK layout engine "this widget
    // wants to consume any spare space in its axis". Without them the editor
    // may collapse to a minimal size inside the Paned.
    source_view.set_vexpand(true);
    source_view.set_hexpand(true);

    // LEARN: ScrolledWindow adds scroll bars when content overflows.
    // PolicyType::Automatic means "only show the scrollbar when needed".
    let editor_scroll = ScrolledWindow::new();
    editor_scroll.set_policy(PolicyType::Automatic, PolicyType::Automatic);
    editor_scroll.set_child(Some(&source_view));
    editor_scroll.set_vexpand(true);
    editor_scroll.set_hexpand(true);

    // ── WebKit preview (right pane) ─────────────────────────────────────────

    // LEARN: WebView::new() creates an embedded browser widget. It spawns a
    // separate web process under the hood (sandboxed), but we interact with it
    // purely through load_html() — no JavaScript needed.
    let web_view = WebView::new();
    web_view.set_vexpand(true);
    web_view.set_hexpand(true);

    // ── Horizontal split (Paned) ────────────────────────────────────────────

    // LEARN: gtk4::Paned is a two-child container with a draggable divider.
    // Orientation::Horizontal places children side by side (left | right).
    let paned = gtk4::Paned::new(Orientation::Horizontal);
    paned.set_start_child(Some(&editor_scroll)); // left
    paned.set_end_child(Some(&web_view)); // right

    // set_wide_handle makes the drag handle easier to grab — good UX.
    paned.set_wide_handle(true);

    // Position the divider at 600px from the left edge (50/50 in a 1200 window).
    // The user can drag it at runtime; this is just the starting position.
    paned.set_position(600);

    paned.set_vexpand(true);
    paned.set_hexpand(true);

    // ── ToolbarView (header + content wrapper) ──────────────────────────────

    // LEARN: adw::ToolbarView is the modern libadwaita way to combine a
    // HeaderBar with page content. It handles the visual overlap between the
    // header and content area so Adwaita's flat header style looks correct.
    // Without it you'd see rendering glitches at the header/content border.
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header_bar);
    toolbar_view.set_content(Some(&paned));

    // ── Menu bar (File | Edit) ──────────────────────────────────────────────
    let menu_model = gio::Menu::new();

    let file_menu = gio::Menu::new();
    file_menu.append(Some("Open"), Some("win.open"));
    file_menu.append(Some("Save"), Some("win.save"));
    file_menu.append(Some("Quit"), Some("win.quit"));
    menu_model.append_submenu(Some("File"), &file_menu);

    let edit_menu = gio::Menu::new();
    edit_menu.append(Some("Undo"), Some("win.undo"));
    edit_menu.append(Some("Redo"), Some("win.redo"));
    menu_model.append_submenu(Some("Edit"), &edit_menu);

    let view_menu = gio::Menu::new();
    view_menu.append(Some("Editor Only"), Some("win.editor-only"));
    view_menu.append(Some("Preview Only"), Some("win.preview-only"));
    view_menu.append(Some("Split View"), Some("win.split-view"));
    menu_model.append_submenu(Some("View"), &view_menu);

    let menu_bar = gtk4::PopoverMenuBar::from_model(Some(&menu_model));
    toolbar_view.add_top_bar(&menu_bar);

    // ── Application window ──────────────────────────────────────────────────

    // LEARN: adw::ApplicationWindow is the top-level window. Using the Adwaita
    // variant (not gtk4::ApplicationWindow) gives you rounded corners, correct
    // shadow treatment, and GNOME Shell integration. It also calls adw::init()
    // automatically so Adwaita is fully initialized before any widget is shown.
    let window = adw::ApplicationWindow::new(app);

    // Show the filename in the title bar when a file is open.
    let title = match file_path {
        Some(path) => {
            let filename = std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(path);
            format!("markdown_smith — {filename}")
        }
        None => "markdown_smith".to_string(),
    };
    window.set_title(Some(&title));

    // set_default_size is the initial pixel size. The user can resize freely.
    window.set_default_size(1200, 800);

    // LEARN: adw::ApplicationWindow uses set_content() (from AdwApplicationWindowExt)
    // instead of gtk4's set_child(). Pass the ToolbarView as the sole content.
    window.set_content(Some(&toolbar_view));

    // ── Shared state ────────────────────────────────────────────────────────
    let current_file: Rc<RefCell<Option<String>>> =
        Rc::new(RefCell::new(file_path.map(str::to_owned)));
    let pane_mode = Rc::new(RefCell::new(PaneMode::Split));
    let last_split_position = Rc::new(RefCell::new(paned.position()));
    let allow_close = Rc::new(RefCell::new(false));

    // ── Action: File → Open ─────────────────────────────────────────────────
    let open_action = gio::SimpleAction::new("open", None);
    {
        let buf = source_buffer.clone();
        let win = window.clone();
        let cf = current_file.clone();
        open_action.connect_activate(move |_, _| {
            let buf = buf.clone();
            let win = win.clone();
            let cf = cf.clone();
            // win_cb is moved into the callback; win is only borrowed for the
            // duration of the dialog.open() call itself (to set the parent window).
            let win_cb = win.clone();
            let dialog = gtk4::FileDialog::new();
            dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        match std::fs::read_to_string(&path) {
                            Ok(content) => {
                                buf.set_text(&content);
                                buf.set_modified(false);
                                let name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown");
                                win_cb.set_title(Some(&format!("markdown_smith — {name}")));
                                *cf.borrow_mut() = Some(path.to_string_lossy().into_owned());
                            }
                            Err(e) => eprintln!("Open error: {e}"),
                        }
                    }
                }
            });
        });
    }
    window.add_action(&open_action);

    // ── Action: File → Save ─────────────────────────────────────────────────
    let save_action = gio::SimpleAction::new("save", None);
    {
        let buf = source_buffer.clone();
        let win = window.clone();
        let cf = current_file.clone();
        save_action.connect_activate(move |_, _| {
            save_buffer_or_prompt_for_path(buf.clone(), win.clone(), cf.clone(), || {});
        });
    }
    window.add_action(&save_action);

    // ── Action: File → Quit ─────────────────────────────────────────────────
    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let buf = source_buffer.clone();
        let win = window.clone();
        let cf = current_file.clone();
        let close_flag = allow_close.clone();
        quit_action.connect_activate(move |_, _| {
            request_quit(buf.clone(), win.clone(), cf.clone(), close_flag.clone());
        });
    }
    window.add_action(&quit_action);

    {
        let buf = source_buffer.clone();
        let cf = current_file.clone();
        let close_flag = allow_close.clone();
        window.connect_close_request(move |win| {
            if *close_flag.borrow() || !buf.is_modified() {
                return gtk4::glib::Propagation::Proceed;
            }

            request_quit(buf.clone(), win.clone(), cf.clone(), close_flag.clone());
            gtk4::glib::Propagation::Stop
        });
    }

    // ── Action: Edit → Undo ─────────────────────────────────────────────────
    let undo_action = gio::SimpleAction::new("undo", None);
    {
        let buf = source_buffer.clone();
        undo_action.connect_activate(move |_, _| {
            if buf.can_undo() {
                buf.undo();
            }
        });
    }
    window.add_action(&undo_action);

    // ── Action: Edit → Redo ─────────────────────────────────────────────────
    let redo_action = gio::SimpleAction::new("redo", None);
    {
        let buf = source_buffer.clone();
        redo_action.connect_activate(move |_, _| {
            if buf.can_redo() {
                buf.redo();
            }
        });
    }
    window.add_action(&redo_action);

    // ── Actions: View → pane modes ──────────────────────────────────────────
    let editor_only_action = gio::SimpleAction::new("editor-only", None);
    {
        let mode = pane_mode.clone();
        let last_position = last_split_position.clone();
        let paned = paned.clone();
        let editor = editor_scroll.clone();
        let preview = web_view.clone();
        editor_only_action.connect_activate(move |_, _| {
            set_pane_mode(
                PaneMode::EditorOnly,
                &mode,
                &last_position,
                &paned,
                &editor,
                &preview,
            );
        });
    }
    window.add_action(&editor_only_action);

    let preview_only_action = gio::SimpleAction::new("preview-only", None);
    {
        let mode = pane_mode.clone();
        let last_position = last_split_position.clone();
        let paned = paned.clone();
        let editor = editor_scroll.clone();
        let preview = web_view.clone();
        preview_only_action.connect_activate(move |_, _| {
            set_pane_mode(
                PaneMode::PreviewOnly,
                &mode,
                &last_position,
                &paned,
                &editor,
                &preview,
            );
        });
    }
    window.add_action(&preview_only_action);

    let split_view_action = gio::SimpleAction::new("split-view", None);
    {
        let mode = pane_mode.clone();
        let last_position = last_split_position.clone();
        let paned = paned.clone();
        let editor = editor_scroll.clone();
        let preview = web_view.clone();
        split_view_action.connect_activate(move |_, _| {
            set_pane_mode(
                PaneMode::Split,
                &mode,
                &last_position,
                &paned,
                &editor,
                &preview,
            );
        });
    }
    window.add_action(&split_view_action);

    apply_configured_shortcuts(app);

    // ── Signal: buffer changed → re-render preview ──────────────────────────

    // LEARN: Signals are GTK's event/observer system. "changed" fires on every
    // buffer modification (keystroke, paste, undo, etc.). We connect a closure
    // that runs the markdown→HTML pipeline and reloads the WebView.
    //
    // LEARN: Why clone? The closure must *own* web_view (because it outlives
    // this function via the signal registration). Rust's ownership rules
    // prevent moving web_view into the closure while we still use it above.
    // Cloning a GObject is cheap — it just increments a reference counter.
    let web_view_clone = web_view.clone();
    source_buffer.connect_changed(move |buffer| {
        // LEARN: buffer.bounds() returns (start_iter, end_iter) in one call.
        // A TextIter is a cursor position inside the text buffer.
        let (start, end) = buffer.bounds();

        // LEARN: buffer.text() extracts the UTF-8 string between two iterators.
        // false = exclude hidden characters (invisible spans used by some
        // widgets — irrelevant here). Returns a glib::GString that derefs to &str.
        let markdown_text = buffer.text(&start, &end, false);

        // Convert and reload. load_html(content, base_uri):
        //   base_uri = None means no base URL for relative resources,
        //   which is fine since our CSS is inline.
        web_view_clone.load_html(&markdown_to_html(markdown_text.as_str()), None);
    });

    // ── Load initial content into the buffer ────────────────────────────────

    // LEARN: set_text fires the "changed" signal synchronously, so the preview
    // is populated via the handler we just connected above — no separate
    // web_view.load_html() call needed.
    let initial_text = match file_path {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => format!("# Could not open file\n\n`{path}`\n\n{e}\n"),
        },
        None => concat!(
            "# Welcome to markdown_smith\n\n",
            "Start typing on the left to see a live preview here.\n\n",
            "---\n\n",
            "**Bold**, *italic*, `inline code`, and [links](https://rust-lang.org) are supported.\n\n",
            "```rust\nfn main() {\n    println!(\"Hello, world!\");\n}\n```\n",
        ).to_string(),
    };
    source_buffer.set_text(&initial_text);
    source_buffer.set_modified(false);

    // ── Show the window ─────────────────────────────────────────────────────

    // LEARN: present() makes the window visible and raises it to the front.
    // In GTK4, child widgets are visible by default once the window is shown,
    // so we do not need to call show() on each individual widget.
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known_actions() -> HashSet<&'static str> {
        DEFAULT_SHORTCUTS
            .iter()
            .map(|(action, _)| *action)
            .collect::<HashSet<_>>()
    }

    fn parse_for_test(contents: &str) -> HashMap<&'static str, String> {
        parse_shortcut_config_with_validator(contents, &known_actions(), |accel| {
            accel.starts_with('<')
        })
    }

    #[test]
    fn parses_valid_shortcut_lines() {
        let shortcuts = parse_for_test("win.editor-only=<Control>e\nwin.preview-only=<Control>p\n");

        assert_eq!(
            shortcuts.get("win.editor-only"),
            Some(&"<Control>e".to_string())
        );
        assert_eq!(
            shortcuts.get("win.preview-only"),
            Some(&"<Control>p".to_string())
        );
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let shortcuts = parse_for_test("\n# comment\n  \nwin.split-view=<Control>0\n");

        assert_eq!(shortcuts.len(), 1);
        assert_eq!(
            shortcuts.get("win.split-view"),
            Some(&"<Control>0".to_string())
        );
    }

    #[test]
    fn ignores_unknown_actions() {
        let shortcuts = parse_for_test("win.unknown=<Control>u\nwin.open=<Control>o\n");

        assert!(!shortcuts.contains_key("win.unknown"));
        assert_eq!(shortcuts.get("win.open"), Some(&"<Control>o".to_string()));
    }

    #[test]
    fn rejects_invalid_accelerators() {
        let shortcuts = parse_for_test("win.open=not a shortcut\nwin.save=<Control>s\n");

        assert!(!shortcuts.contains_key("win.open"));
        assert_eq!(shortcuts.get("win.save"), Some(&"<Control>s".to_string()));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ENTRY POINT
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    // LEARN: adw::Application wraps gio::Application and gtk4::Application.
    // Prefer it over gtk4::Application in any Adwaita app because it:
    //   • calls adw::init() automatically (sets up HIG styles & icon theme)
    //   • configures the style manager for dark/light mode
    //   • registers with D-Bus for single-instance behaviour
    //
    // The app-id "com.example.markdown-smith" is a reverse-DNS identifier that
    // must be unique on the system. Replace "example" with your own domain for
    // a real app that you intend to distribute.
    // Read the optional file path from the first non-flag argument.
    // We do this before app.run() because GTK strips its own flags
    // (--display, --class, etc.) during run() and we'd lose our argument.
    let file_path: Option<String> = std::env::args().skip(1).find(|a| !a.starts_with('-'));

    let app = Application::new(
        Some("com.example.markdown-smith"),
        gtk4::gio::ApplicationFlags::empty(),
    );

    // LEARN: connect_activate wires the "activate" signal to build_ui.
    // "activate" fires when the app is ready to show its first window.
    // All widget creation should happen inside this callback, not before.
    //
    // We capture file_path by move into the closure. as_deref() converts
    // Option<String> → Option<&str> so build_ui doesn't need to own the String.
    app.connect_activate(move |app| build_ui(app, file_path.as_deref()));

    // LEARN: run_with_args passes an explicit (empty) argument list to GTK
    // instead of forwarding std::env::args(). Without this, GTK sees our
    // filename and tries to open it via the GIO "open files" mechanism, which
    // we haven't enabled — producing a "can not open files" critical warning.
    // We already captured the file path above, so GTK doesn't need to see it.
    let exit_code = app.run_with_args::<&str>(&[]);

    // Convert glib::ExitCode to a plain process exit code for the shell.
    std::process::exit(exit_code.into());
}

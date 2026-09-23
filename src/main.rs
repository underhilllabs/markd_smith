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
use adw::{Application, ColorScheme, StyleManager};

// LEARN: We only import the gtk4 types we reference by name. Everything else
// comes in through the prelude traits above.
use gtk4::{gdk, EventControllerKey, PropagationPhase};
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
use pulldown_cmark::{html, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

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
    ("win.swap-panes", "<Control><Shift>x"),
    ("win.zoom-in", "<Control>plus"),
    ("win.zoom-out", "<Control>minus"),
    ("win.zoom-reset", "<Control><Shift>0"),
];

// Extra accelerators registered alongside the configured one, never written to
// shortcuts.conf. On most layouts "+" is Shift+=, so a bare Ctrl+= must also
// zoom in — that is what every browser does — and the numeric keypad sends its
// own distinct keysyms.
const SHORTCUT_ALIASES: &[(&str, &str)] = &[
    ("win.zoom-in", "<Control>equal"),
    ("win.zoom-in", "<Control>KP_Add"),
    ("win.zoom-out", "<Control>KP_Subtract"),
    ("win.zoom-reset", "<Control>KP_0"),
];

// Zoom is a multiplier applied to both panes at once. The bounds keep the UI
// usable at the extremes; the step matches the ~10% increments browsers use.
const ZOOM_MIN: f64 = 0.5;
const ZOOM_MAX: f64 = 3.0;
const ZOOM_STEP: f64 = 1.1;
const ZOOM_DEFAULT: f64 = 1.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PaneMode {
    Split,
    EditorOnly,
    PreviewOnly,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThemeMode {
    Light,
    Dark,
}

impl ThemeMode {
    fn as_config_value(self) -> &'static str {
        match self {
            ThemeMode::Light => "light",
            ThemeMode::Dark => "dark",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// MARKDOWN → HTML CONVERSION
// ─────────────────────────────────────────────────────────────────────────────

// LEARN: include_str! embeds a file's contents into the binary at compile
// time, so the preview renders diagrams with no network access and no
// dependency on a CDN staying online. The bundle is a classic (non-module)
// script that assigns globalThis.mermaid, so a plain <script> tag works.
const MERMAID_JS: &str = include_str!("../assets/mermaid.min.js");

// Minimal HTML escaping for text we drop into the document verbatim. Mermaid
// reads the diagram source as the element's text content, so `A --> B` must
// not be mistaken for markup.
fn escape_html(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            other => escaped.push(other),
        }
    }
    escaped
}

// Render the markdown body, turning ```mermaid fences into the `<pre
// class="mermaid">` blocks mermaid.js looks for instead of syntax-highlighted
// code. Returns the HTML fragment plus whether any diagram was found, so the
// caller only pays for the (large) mermaid bundle on documents that use it.
//
// LEARN: pulldown-cmark hands us a stream of Events. Mapping over that stream
// before it reaches html::push_html is the idiomatic way to special-case one
// kind of node — we swallow the events inside a mermaid fence and emit a
// single Event::Html in their place.
fn render_markdown_body(markdown: &str) -> (String, bool) {
    let options = Options::all();
    let parser = Parser::new_ext(markdown, options);

    let mut events = Vec::new();
    let mut diagram_source: Option<String> = None;
    let mut has_diagram = false;

    for event in parser {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref language)))
                if language.trim().eq_ignore_ascii_case("mermaid") =>
            {
                diagram_source = Some(String::new());
            }
            Event::Text(ref text) if diagram_source.is_some() => {
                // A fenced block can yield several Text events; concatenate them.
                diagram_source
                    .as_mut()
                    .expect("checked by the guard above")
                    .push_str(text);
            }
            Event::End(TagEnd::CodeBlock) if diagram_source.is_some() => {
                let source = diagram_source.take().expect("checked by the guard above");
                has_diagram = true;
                events.push(Event::Html(
                    format!(
                        "<pre class=\"mermaid\">{}</pre>\n",
                        escape_html(source.trim_end())
                    )
                    .into(),
                ));
            }
            other => events.push(other),
        }
    }

    let mut html_output = String::new();
    html::push_html(&mut html_output, events.into_iter());
    (html_output, has_diagram)
}

// LEARN: This function is pure — it has nothing to do with GTK. It takes a
// &str (a borrowed string slice) and returns an owned String. It is called
// from inside a GTK signal handler but is independently testable.
fn markdown_to_html(markdown: &str, theme_mode: ThemeMode) -> String {
    // The body (including any mermaid blocks) is produced by
    // render_markdown_body; this function only wraps it in a themed document.
    let (html_output, has_diagram) = render_markdown_body(markdown);

    let (
        background,
        text,
        pre_background,
        code_background,
        quote_border,
        quote_text,
        table_border,
        table_header_background,
        link,
        mermaid_theme,
    ) = match theme_mode {
        ThemeMode::Light => (
            "#ffffff", "#1c1c1e", "#f5f5f5", "#f0f0f0", "#d0d0d0", "#555", "#ddd", "#f5f5f5",
            "#0062cc", "default",
        ),
        ThemeMode::Dark => (
            "#1e1e1e", "#f2f2f2", "#2b2b2b", "#303030", "#5a5a5a", "#c7c7c7", "#4a4a4a", "#2b2b2b",
            "#8ab4f8", "dark",
        ),
    };

    // Only embed the multi-megabyte mermaid bundle when the document actually
    // contains a diagram — the preview reloads on every keystroke.
    let mermaid_script = if has_diagram {
        format!(
            r#"<script>{MERMAID_JS}</script>
<script>
  mermaid.initialize({{ startOnLoad: true, theme: "{mermaid_theme}", securityLevel: "strict" }});
</script>"#
        )
    } else {
        String::new()
    };

    // Wrap in a minimal HTML document so WebKit gets correct UTF-8 and
    // sensible default typography. The double braces {{ }} are how you write
    // a literal { } inside a Rust format!() string.
    format!(
        r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8">
  <style>
    html {{ background: {background}; }}
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      font-size: 15px;
      line-height: 1.65;
      max-width: 820px;
      margin: 0 auto;
      padding: 1.25rem 2rem;
      background: {background};
      color: {text};
    }}
    h1, h2, h3, h4, h5, h6 {{
      margin-top: 1.5em;
      margin-bottom: 0.4em;
      line-height: 1.25;
    }}
    pre {{
      background: {pre_background};
      padding: 0.9em 1em;
      border-radius: 6px;
      overflow-x: auto;
    }}
    code {{
      font-family: "JetBrains Mono", "Fira Code", monospace;
      font-size: 0.875em;
      background: {code_background};
      padding: 0.15em 0.35em;
      border-radius: 3px;
    }}
    pre code {{ background: none; padding: 0; }}
    blockquote {{
      border-left: 4px solid {quote_border};
      margin: 0;
      padding-left: 1.1em;
      color: {quote_text};
    }}
    table {{ border-collapse: collapse; width: 100%; margin: 1em 0; }}
    th, td {{ border: 1px solid {table_border}; padding: 0.45em 0.75em; }}
    th {{ background: {table_header_background}; font-weight: 600; }}
    a {{ color: {link}; }}
    img {{ max-width: 100%; }}
    pre.mermaid {{
      background: none;
      padding: 0;
      text-align: center;
      /* Hidden until mermaid swaps the source text for an <svg>, so the raw
         diagram definition never flashes on screen mid-render. */
      visibility: hidden;
    }}
    pre.mermaid[data-processed="true"] {{ visibility: visible; }}
    pre.mermaid svg {{ max-width: 100%; height: auto; }}
  </style>
</head>
<body>
{html_output}
{mermaid_script}
</body>
</html>"#
    )
}

fn config_dir() -> Option<PathBuf> {
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(config_home).join("markd_smith"));
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/markdown_smith"))
}

fn settings_config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("settings.conf"))
}

fn parse_theme_settings(contents: &str) -> ThemeMode {
    let mut invalid_theme = None;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        if key.trim() != "theme" {
            continue;
        }

        match value.trim() {
            "light" => return ThemeMode::Light,
            "dark" => return ThemeMode::Dark,
            other => invalid_theme = Some(other.to_string()),
        }
    }

    if let Some(theme) = invalid_theme {
        eprintln!("Invalid theme setting `{theme}`; falling back to light mode");
    }

    ThemeMode::Light
}

fn load_theme_mode() -> ThemeMode {
    let Some(path) = settings_config_path() else {
        return ThemeMode::Light;
    };

    match std::fs::read_to_string(&path) {
        Ok(contents) => parse_theme_settings(&contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ThemeMode::Light,
        Err(err) => {
            eprintln!("Could not read settings config: {err}");
            ThemeMode::Light
        }
    }
}

fn save_theme_mode(theme_mode: ThemeMode) {
    let Some(path) = settings_config_path() else {
        return;
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            eprintln!("Could not create settings config directory: {err}");
            return;
        }
    }

    let contents = format!("theme={}\n", theme_mode.as_config_value());
    if let Err(err) = std::fs::write(&path, contents) {
        eprintln!("Could not write settings config: {err}");
    }
}

// The most-recently-opened files, newest first, are persisted one path per line
// in this file so the "Open Recent" menu survives across launches.
const MAX_RECENT_FILES: usize = 10;

fn recent_files_config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("recent_files"))
}

fn load_recent_files() -> Vec<String> {
    let Some(path) = recent_files_config_path() else {
        return Vec::new();
    };

    match std::fs::read_to_string(&path) {
        Ok(contents) => contents
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            eprintln!("Could not read recent files: {err}");
            Vec::new()
        }
    }
}

fn save_recent_files(paths: &[String]) {
    let Some(path) = recent_files_config_path() else {
        return;
    };

    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            eprintln!("Could not create recent files directory: {err}");
            return;
        }
    }

    // One path per line; a trailing newline keeps the file POSIX-friendly.
    let mut contents = paths.join("\n");
    if !contents.is_empty() {
        contents.push('\n');
    }
    if let Err(err) = std::fs::write(&path, contents) {
        eprintln!("Could not write recent files: {err}");
    }
}

// Pure list transform: move `path` to the front, drop any earlier appearance,
// and cap the length. Split out from I/O so it can be unit-tested.
fn recent_list_with(mut recent: Vec<String>, path: &str, max: usize) -> Vec<String> {
    recent.retain(|existing| existing != path);
    recent.insert(0, path.to_owned());
    recent.truncate(max);
    recent
}

// Move `path` to the front of the recent list, persist it, and return the
// updated list.
fn push_recent_file(path: &str) -> Vec<String> {
    let recent = recent_list_with(load_recent_files(), path, MAX_RECENT_FILES);
    save_recent_files(&recent);
    recent
}

fn shortcut_config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("shortcuts.conf"))
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
        let Some(accel) = shortcuts.get(action) else {
            continue;
        };

        // LEARN: set_accels_for_action takes a *list* — an action can have
        // several accelerators. The configured binding comes first so it is
        // the one GTK shows in menus.
        let mut accels = vec![accel.as_str()];
        accels.extend(
            SHORTCUT_ALIASES
                .iter()
                .filter(|(alias_action, _)| alias_action == action)
                .map(|(_, alias_accel)| *alias_accel),
        );

        app.set_accels_for_action(action, &accels);
    }
}

// Apply a zoom multiplier to both panes.
//
// The preview is easy: WebKit has real zoom that reflows the document. The
// editor has no zoom API, so we scale its font instead through a CSS provider
// installed on the display. Sizing in `em` keeps the user's system font size
// as the baseline rather than hardcoding a point size, and at 100% we clear
// the CSS entirely so the default appearance is untouched.
fn apply_zoom(zoom: f64, web_view: &WebView, editor_css: &gtk4::CssProvider) {
    web_view.set_zoom_level(zoom);

    if (zoom - ZOOM_DEFAULT).abs() < f64::EPSILON {
        editor_css.load_from_string("");
    } else {
        editor_css.load_from_string(&format!("textview {{ font-size: {zoom}em; }}"));
    }
}

// Multiply `current` by `factor`, or reset to 100% when `factor` is None.
// Rounding to two decimals stops repeated multiplication from drifting to
// values like 0.9999999999999999.
fn next_zoom(current: f64, factor: Option<f64>) -> f64 {
    match factor {
        Some(factor) => ((current * factor * 100.0).round() / 100.0).clamp(ZOOM_MIN, ZOOM_MAX),
        None => ZOOM_DEFAULT,
    }
}

// Compute the next zoom level and apply it to both panes.
fn adjust_zoom(
    factor: Option<f64>,
    zoom: &Rc<std::cell::Cell<f64>>,
    web_view: &WebView,
    editor_css: &gtk4::CssProvider,
) {
    let next = next_zoom(zoom.get(), factor);
    zoom.set(next);
    apply_zoom(next, web_view, editor_css);
}

fn set_source_style_scheme_for_theme(
    source_buffer: &Buffer,
    scheme_manager: &StyleSchemeManager,
    theme_mode: ThemeMode,
) {
    let scheme_ids = match theme_mode {
        ThemeMode::Light => ["kate", "Adwaita", "classic"],
        ThemeMode::Dark => ["Adwaita-dark", "solarized-dark", "cobalt"],
    };

    for scheme_id in scheme_ids {
        if let Some(scheme) = scheme_manager.scheme(scheme_id) {
            source_buffer.set_style_scheme(Some(&scheme));
            return;
        }
    }

    eprintln!("Could not find a GtkSourceView style scheme for {theme_mode:?}");
}

fn apply_theme(
    source_buffer: &Buffer,
    scheme_manager: &StyleSchemeManager,
    style_manager: &StyleManager,
    theme_mode: ThemeMode,
) {
    let color_scheme = match theme_mode {
        ThemeMode::Light => ColorScheme::ForceLight,
        ThemeMode::Dark => ColorScheme::ForceDark,
    };

    style_manager.set_color_scheme(color_scheme);
    set_source_style_scheme_for_theme(source_buffer, scheme_manager, theme_mode);
}

fn render_preview(buffer: &Buffer, web_view: &WebView, theme_mode: ThemeMode) {
    let (start, end) = buffer.bounds();
    let markdown_text = buffer.text(&start, &end, false);
    web_view.load_html(&markdown_to_html(markdown_text.as_str(), theme_mode), None);
}

// Abbreviate the user's home directory to "~" so long absolute paths stay
// readable as menu labels while remaining unambiguous across folders.
fn recent_menu_label(path: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy();
        if !home.is_empty() {
            if let Some(rest) = path.strip_prefix(home.as_ref()) {
                return format!("~{rest}");
            }
        }
    }
    path.to_owned()
}

// Replace the contents of the "Open Recent" submenu with one entry per path.
// Each entry targets the parameterized `win.open-recent` action, carrying its
// path as a string GVariant. An empty list shows a disabled placeholder.
fn rebuild_recent_menu(recent_menu: &gio::Menu, paths: &[String]) {
    recent_menu.remove_all();

    if paths.is_empty() {
        // A menu item with no action renders insensitive (greyed out).
        recent_menu.append(Some("(No recent files)"), None);
        return;
    }

    for path in paths {
        let item = gio::MenuItem::new(Some(&recent_menu_label(path)), None);
        item.set_action_and_target_value(Some("win.open-recent"), Some(&path.to_variant()));
        recent_menu.append_item(&item);
    }
}

// Load `path` into the editor: read its contents into the buffer, update the
// window title and the "currently open file" state, and record it in the recent
// files list (rebuilding the Open Recent menu to match). Shared by the Open
// dialog, the Open Recent entries, and the initial file passed on the CLI.
fn open_path_into_editor(
    path: &std::path::Path,
    buffer: &Buffer,
    window: &adw::ApplicationWindow,
    current_file: &Rc<RefCell<Option<String>>>,
    recent_menu: &gio::Menu,
) {
    // Store a canonical absolute path so the recent list de-duplicates entries
    // that were reached via different relative paths.
    let absolute = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    match std::fs::read_to_string(&absolute) {
        Ok(content) => {
            buffer.set_text(&content);
            buffer.set_modified(false);
            let name = absolute
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown");
            window.set_title(Some(&format!("markdown_smith — {name}")));
            let stored = absolute.to_string_lossy().into_owned();
            *current_file.borrow_mut() = Some(stored.clone());
            let recent = push_recent_file(&stored);
            rebuild_recent_menu(recent_menu, &recent);
        }
        Err(err) => {
            eprintln!("Open error: {err}");
            // A recent entry that no longer opens (moved/deleted) is dropped so
            // it stops cluttering the menu.
            let stored = absolute.to_string_lossy().into_owned();
            let mut recent = load_recent_files();
            let before = recent.len();
            recent.retain(|existing| existing != &stored);
            if recent.len() != before {
                save_recent_files(&recent);
                rebuild_recent_menu(recent_menu, &recent);
            }
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

// LEARN: gtk4::Paned identifies its two children by position (start/end), not
// by identity — swapping which widget occupies which slot is enough to flip
// the visual layout. The divider position (in pixels from the left edge) is
// left untouched, so the split ratio looks the same after swapping.
fn swap_panes(
    panes_swapped: &Rc<RefCell<bool>>,
    paned: &gtk4::Paned,
    editor_scroll: &ScrolledWindow,
    web_view: &WebView,
) {
    let swapped = !*panes_swapped.borrow();
    *panes_swapped.borrow_mut() = swapped;

    // LEARN: Paned::set_*_child asserts the child is unparented (or already
    // in that slot) before accepting it — so a widget can't move directly
    // from one slot to the other. Clear both slots first, then reassign.
    paned.set_start_child(gtk4::Widget::NONE);
    paned.set_end_child(gtk4::Widget::NONE);

    if swapped {
        paned.set_start_child(Some(web_view));
        paned.set_end_child(Some(editor_scroll));
    } else {
        paned.set_start_child(Some(editor_scroll));
        paned.set_end_child(Some(web_view));
    }
}

#[allow(clippy::too_many_arguments)]
fn install_focus_pane_shortcuts(
    window: &adw::ApplicationWindow,
    source_view: &View,
    web_view: &WebView,
    pane_mode: Rc<RefCell<PaneMode>>,
    last_split_position: Rc<RefCell<i32>>,
    panes_swapped: Rc<RefCell<bool>>,
    paned: gtk4::Paned,
    editor_scroll: ScrolledWindow,
) {
    let key_controller = EventControllerKey::new();
    key_controller.set_propagation_phase(PropagationPhase::Capture);

    let chord_pending = Rc::new(RefCell::new(false));
    {
        let chord_pending = chord_pending.clone();
        let editor = source_view.clone();
        let preview = web_view.clone();
        key_controller.connect_key_pressed(move |_, key, _, state| {
            if *chord_pending.borrow() {
                *chord_pending.borrow_mut() = false;

                // Panes can be swapped (View → Swap Panes), so "jump left"/
                // "jump right" must resolve to whichever widget currently
                // occupies that side rather than a fixed editor/preview pairing.
                let (left_widget, right_widget): (&gtk4::Widget, &gtk4::Widget) =
                    if *panes_swapped.borrow() {
                        (preview.upcast_ref(), editor.upcast_ref())
                    } else {
                        (editor.upcast_ref(), preview.upcast_ref())
                    };

                match key {
                    gdk::Key::Left => {
                        set_pane_mode(
                            PaneMode::Split,
                            &pane_mode,
                            &last_split_position,
                            &paned,
                            &editor_scroll,
                            &preview,
                        );
                        left_widget.grab_focus();
                        gtk4::glib::Propagation::Stop
                    }
                    gdk::Key::Right => {
                        set_pane_mode(
                            PaneMode::Split,
                            &pane_mode,
                            &last_split_position,
                            &paned,
                            &editor_scroll,
                            &preview,
                        );
                        right_widget.grab_focus();
                        gtk4::glib::Propagation::Stop
                    }
                    _ => gtk4::glib::Propagation::Proceed,
                }
            } else if state.contains(gdk::ModifierType::CONTROL_MASK)
                && matches!(key, gdk::Key::w | gdk::Key::W)
            {
                *chord_pending.borrow_mut() = true;
                gtk4::glib::Propagation::Stop
            } else {
                gtk4::glib::Propagation::Proceed
            }
        });
    }

    window.add_controller(key_controller);
}

// ─────────────────────────────────────────────────────────────────────────────
// UI CONSTRUCTION
// ─────────────────────────────────────────────────────────────────────────────

// LEARN: gio::Menu (unlike gtk4 widgets) needs no display connection to
// construct or inspect, which makes the menu structure itself unit-testable
// — see `tests::menu_bar_has_expected_structure` below.
// Returns the full menu model plus a handle to the (initially empty) "Open
// Recent" submenu, so the caller can repopulate it as files are opened.
fn build_menu_model() -> (gio::Menu, gio::Menu) {
    let menu_model = gio::Menu::new();

    let file_menu = gio::Menu::new();
    file_menu.append(Some("Open"), Some("win.open"));
    let recent_menu = gio::Menu::new();
    file_menu.append_submenu(Some("Open Recent"), &recent_menu);
    file_menu.append(Some("Save"), Some("win.save"));
    file_menu.append(Some("Quit"), Some("win.quit"));
    menu_model.append_submenu(Some("File"), &file_menu);

    let edit_menu = gio::Menu::new();
    edit_menu.append(Some("Undo"), Some("win.undo"));
    edit_menu.append(Some("Redo"), Some("win.redo"));
    menu_model.append_submenu(Some("Edit"), &edit_menu);

    let view_menu = gio::Menu::new();
    view_menu.append(Some("Light Mode"), Some("win.light-mode"));
    view_menu.append(Some("Dark Mode"), Some("win.dark-mode"));
    view_menu.append(Some("Editor Only"), Some("win.editor-only"));
    view_menu.append(Some("Preview Only"), Some("win.preview-only"));
    view_menu.append(Some("Split View"), Some("win.split-view"));
    view_menu.append(Some("Swap Panes"), Some("win.swap-panes"));
    view_menu.append(Some("Zoom In"), Some("win.zoom-in"));
    view_menu.append(Some("Zoom Out"), Some("win.zoom-out"));
    view_menu.append(Some("Reset Zoom"), Some("win.zoom-reset"));
    menu_model.append_submenu(Some("View"), &view_menu);

    (menu_model, recent_menu)
}

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

    // LEARN: StyleSchemeManager is another singleton. We use it below to keep
    // the editor color scheme in sync with the app's light/dark theme.
    let scheme_manager = StyleSchemeManager::default();
    let style_manager = StyleManager::default();
    let initial_theme_mode = load_theme_mode();
    apply_theme(
        &source_buffer,
        &scheme_manager,
        &style_manager,
        initial_theme_mode,
    );

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
    web_view.set_focusable(true);
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
    let (menu_model, recent_menu) = build_menu_model();
    rebuild_recent_menu(&recent_menu, &load_recent_files());

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
    let theme_mode = Rc::new(RefCell::new(initial_theme_mode));
    let last_split_position = Rc::new(RefCell::new(paned.position()));
    let panes_swapped = Rc::new(RefCell::new(false));
    let allow_close = Rc::new(RefCell::new(false));

    install_focus_pane_shortcuts(
        &window,
        &source_view,
        &web_view,
        pane_mode.clone(),
        last_split_position.clone(),
        panes_swapped.clone(),
        paned.clone(),
        editor_scroll.clone(),
    );

    // ── Action: File → Open ─────────────────────────────────────────────────
    let open_action = gio::SimpleAction::new("open", None);
    {
        let buf = source_buffer.clone();
        let win = window.clone();
        let cf = current_file.clone();
        let recent = recent_menu.clone();
        open_action.connect_activate(move |_, _| {
            let buf = buf.clone();
            let cf = cf.clone();
            let recent = recent.clone();
            // win_cb is moved into the callback; win is only borrowed for the
            // duration of the dialog.open() call itself (to set the parent window).
            let win_cb = win.clone();
            let dialog = gtk4::FileDialog::new();
            // Open in the folder of the current file so the common case —
            // reaching for a sibling document — needs no navigation.
            if let Some(dir) = cf
                .borrow()
                .as_ref()
                .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
            {
                dialog.set_initial_folder(Some(&gio::File::for_path(&dir)));
            }
            dialog.open(Some(&win), gio::Cancellable::NONE, move |result| {
                if let Ok(file) = result {
                    if let Some(path) = file.path() {
                        open_path_into_editor(&path, &buf, &win_cb, &cf, &recent);
                    }
                }
            });
        });
    }
    window.add_action(&open_action);

    // ── Action: File → Open Recent → <path> ─────────────────────────────────
    // A single parameterized action serves every entry in the Open Recent
    // submenu; the target GVariant carries which path to open.
    let open_recent_action =
        gio::SimpleAction::new("open-recent", Some(gtk4::glib::VariantTy::STRING));
    {
        let buf = source_buffer.clone();
        let win = window.clone();
        let cf = current_file.clone();
        let recent = recent_menu.clone();
        open_recent_action.connect_activate(move |_, param| {
            let Some(path) = param.and_then(|value| value.get::<String>()) else {
                return;
            };
            open_path_into_editor(std::path::Path::new(&path), &buf, &win, &cf, &recent);
        });
    }
    window.add_action(&open_recent_action);

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

    // ── Actions: View → theme modes ─────────────────────────────────────────
    let light_mode_action = gio::SimpleAction::new("light-mode", None);
    {
        let mode = theme_mode.clone();
        let buf = source_buffer.clone();
        let preview = web_view.clone();
        let schemes = scheme_manager.clone();
        let styles = style_manager.clone();
        light_mode_action.connect_activate(move |_, _| {
            *mode.borrow_mut() = ThemeMode::Light;
            apply_theme(&buf, &schemes, &styles, ThemeMode::Light);
            render_preview(&buf, &preview, ThemeMode::Light);
            save_theme_mode(ThemeMode::Light);
        });
    }
    window.add_action(&light_mode_action);

    let dark_mode_action = gio::SimpleAction::new("dark-mode", None);
    {
        let mode = theme_mode.clone();
        let buf = source_buffer.clone();
        let preview = web_view.clone();
        let schemes = scheme_manager.clone();
        let styles = style_manager.clone();
        dark_mode_action.connect_activate(move |_, _| {
            *mode.borrow_mut() = ThemeMode::Dark;
            apply_theme(&buf, &schemes, &styles, ThemeMode::Dark);
            render_preview(&buf, &preview, ThemeMode::Dark);
            save_theme_mode(ThemeMode::Dark);
        });
    }
    window.add_action(&dark_mode_action);

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

    let swap_panes_action = gio::SimpleAction::new("swap-panes", None);
    {
        let panes_swapped = panes_swapped.clone();
        let paned = paned.clone();
        let editor = editor_scroll.clone();
        let preview = web_view.clone();
        swap_panes_action.connect_activate(move |_, _| {
            swap_panes(&panes_swapped, &paned, &editor, &preview);
        });
    }
    window.add_action(&swap_panes_action);

    // ── Zoom ────────────────────────────────────────────────────────────────

    // LEARN: Cell<f64> gives interior mutability for a Copy type without the
    // borrow bookkeeping of RefCell — the zoom level is a single number that
    // several closures need to read and write.
    let zoom_level = Rc::new(std::cell::Cell::new(ZOOM_DEFAULT));

    // LEARN: A CssProvider added to the *display* styles every widget in the
    // app. We keep a handle so each zoom step can rewrite its contents in
    // place. APPLICATION priority sits above the theme but below user CSS.
    let editor_css = gtk4::CssProvider::new();
    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &editor_css,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    for (action_name, factor) in [
        ("zoom-in", Some(ZOOM_STEP)),
        ("zoom-out", Some(1.0 / ZOOM_STEP)),
        ("zoom-reset", None),
    ] {
        let action = gio::SimpleAction::new(action_name, None);
        let zoom = zoom_level.clone();
        let preview = web_view.clone();
        let css = editor_css.clone();
        action.connect_activate(move |_, _| {
            adjust_zoom(factor, &zoom, &preview, &css);
        });
        window.add_action(&action);
    }

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
    let theme_mode_clone = theme_mode.clone();
    source_buffer.connect_changed(move |buffer| {
        // Convert and reload. load_html(content, base_uri):
        //   base_uri = None means no base URL for relative resources,
        //   which is fine since our CSS is inline.
        render_preview(buffer, &web_view_clone, *theme_mode_clone.borrow());
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

    // A file passed on the command line counts as "recently opened" too.
    if let Some(path) = file_path {
        let absolute =
            std::fs::canonicalize(path).unwrap_or_else(|_| std::path::PathBuf::from(path));
        if absolute.is_file() {
            let recent = push_recent_file(&absolute.to_string_lossy());
            rebuild_recent_menu(&recent_menu, &recent);
        }
    }

    // ── Show the window ─────────────────────────────────────────────────────

    // LEARN: present() makes the window visible and raises it to the front.
    // In GTK4, child widgets are visible by default once the window is shown,
    // so we do not need to call show() on each individual widget.
    window.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_steps_up_and_down_and_returns_to_the_default() {
        let zoomed_in = next_zoom(ZOOM_DEFAULT, Some(ZOOM_STEP));
        assert!(zoomed_in > ZOOM_DEFAULT);

        // Stepping back down lands exactly on 1.0 rather than drifting.
        assert_eq!(next_zoom(zoomed_in, Some(1.0 / ZOOM_STEP)), ZOOM_DEFAULT);
        assert_eq!(next_zoom(zoomed_in, None), ZOOM_DEFAULT);
    }

    #[test]
    fn zoom_is_clamped_to_its_bounds() {
        assert_eq!(next_zoom(ZOOM_MAX, Some(ZOOM_STEP)), ZOOM_MAX);
        assert_eq!(next_zoom(ZOOM_MIN, Some(1.0 / ZOOM_STEP)), ZOOM_MIN);
    }

    #[test]
    fn shortcut_aliases_only_target_known_actions() {
        let actions = known_actions();
        for (action, _) in SHORTCUT_ALIASES {
            assert!(actions.contains(action), "unknown alias action `{action}`");
        }
    }

    #[test]
    fn renders_mermaid_fences_as_diagram_blocks() {
        let (body, has_diagram) = render_markdown_body("```mermaid\ngraph TD;\n  A --> B;\n```\n");

        assert!(has_diagram);
        assert!(body.contains("<pre class=\"mermaid\">"));
        // The arrow must survive as escaped text, not become markup.
        assert!(body.contains("A --&gt; B;"));
        assert!(!body.contains("<code>"));
    }

    #[test]
    fn renders_other_fenced_code_blocks_normally() {
        let (body, has_diagram) = render_markdown_body("```rust\nfn main() {}\n```\n");

        assert!(!has_diagram);
        assert!(body.contains("<code class=\"language-rust\">"));
        assert!(!body.contains("class=\"mermaid\""));
    }

    #[test]
    fn embeds_mermaid_bundle_only_when_a_diagram_is_present() {
        let with_diagram =
            markdown_to_html("```mermaid\ngraph TD;\n  A --> B;\n```", ThemeMode::Light);
        let without_diagram = markdown_to_html("# Just a heading", ThemeMode::Light);

        assert!(with_diagram.contains("mermaid.initialize"));
        assert!(!without_diagram.contains("mermaid.initialize"));
    }

    #[test]
    fn selects_mermaid_theme_matching_the_app_theme() {
        let dark = markdown_to_html("```mermaid\ngraph TD;\n  A --> B;\n```", ThemeMode::Dark);

        assert!(dark.contains(r#"theme: "dark""#));
    }

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
    fn parses_light_theme_setting() {
        assert_eq!(parse_theme_settings("theme=light\n"), ThemeMode::Light);
    }

    #[test]
    fn parses_dark_theme_setting() {
        assert_eq!(parse_theme_settings("theme=dark\n"), ThemeMode::Dark);
    }

    #[test]
    fn theme_settings_ignore_comments_and_blank_lines() {
        assert_eq!(
            parse_theme_settings("\n# comment\n  \ntheme=dark\n"),
            ThemeMode::Dark
        );
    }

    #[test]
    fn theme_settings_fall_back_to_light_when_missing_or_invalid() {
        assert_eq!(parse_theme_settings(""), ThemeMode::Light);
        assert_eq!(parse_theme_settings("theme=blue\n"), ThemeMode::Light);
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

    // Walks a gio::MenuModel's top-level submenus, returning
    // (submenu_label, [(item_label, action_name), ...]) pairs in order.
    fn menu_structure(menu: &gio::Menu) -> Vec<(String, Vec<(String, String)>)> {
        let attr_string = |menu: &gio::MenuModel, index: i32, attribute: &str| -> Option<String> {
            menu.item_attribute_value(index, attribute, Some(gtk4::glib::VariantTy::STRING))
                .and_then(|v| v.get::<String>())
        };

        (0..menu.n_items())
            .map(|i| {
                let label = attr_string(menu.upcast_ref(), i, "label").unwrap_or_default();
                let submenu = menu
                    .item_link(i, "submenu")
                    .expect("top-level menu item should be a submenu");

                let items = (0..submenu.n_items())
                    .map(|j| {
                        let item_label = attr_string(&submenu, j, "label").unwrap_or_default();
                        let action = attr_string(&submenu, j, "action").unwrap_or_default();
                        (item_label, action)
                    })
                    .collect();

                (label, items)
            })
            .collect()
    }

    #[test]
    fn recent_list_moves_repeat_to_front_without_duplicating() {
        let existing = vec!["/a.md".to_string(), "/b.md".to_string()];
        assert_eq!(
            recent_list_with(existing, "/b.md", 10),
            vec!["/b.md".to_string(), "/a.md".to_string()],
        );
    }

    #[test]
    fn recent_list_prepends_new_entries() {
        let existing = vec!["/a.md".to_string()];
        assert_eq!(
            recent_list_with(existing, "/b.md", 10),
            vec!["/b.md".to_string(), "/a.md".to_string()],
        );
    }

    #[test]
    fn recent_list_caps_at_max_length() {
        let existing = vec!["/1".to_string(), "/2".to_string(), "/3".to_string()];
        assert_eq!(
            recent_list_with(existing, "/new", 2),
            vec!["/new".to_string(), "/1".to_string()],
        );
    }

    #[test]
    fn recent_menu_label_abbreviates_home_directory() {
        // recent_menu_label reads $HOME; drive it with a known value.
        std::env::set_var("HOME", "/home/tester");
        assert_eq!(
            recent_menu_label("/home/tester/docs/notes.md"),
            "~/docs/notes.md"
        );
        assert_eq!(recent_menu_label("/etc/hosts"), "/etc/hosts");
    }

    #[test]
    fn menu_bar_has_expected_structure() {
        let structure = menu_structure(&build_menu_model().0);

        assert_eq!(
            structure,
            vec![
                (
                    "File".to_string(),
                    vec![
                        ("Open".to_string(), "win.open".to_string()),
                        // "Open Recent" is a submenu, so it carries no action.
                        ("Open Recent".to_string(), String::new()),
                        ("Save".to_string(), "win.save".to_string()),
                        ("Quit".to_string(), "win.quit".to_string()),
                    ]
                ),
                (
                    "Edit".to_string(),
                    vec![
                        ("Undo".to_string(), "win.undo".to_string()),
                        ("Redo".to_string(), "win.redo".to_string()),
                    ]
                ),
                (
                    "View".to_string(),
                    vec![
                        ("Light Mode".to_string(), "win.light-mode".to_string()),
                        ("Dark Mode".to_string(), "win.dark-mode".to_string()),
                        ("Editor Only".to_string(), "win.editor-only".to_string()),
                        ("Preview Only".to_string(), "win.preview-only".to_string()),
                        ("Split View".to_string(), "win.split-view".to_string()),
                        ("Swap Panes".to_string(), "win.swap-panes".to_string()),
                        ("Zoom In".to_string(), "win.zoom-in".to_string()),
                        ("Zoom Out".to_string(), "win.zoom-out".to_string()),
                        ("Reset Zoom".to_string(), "win.zoom-reset".to_string()),
                    ]
                ),
            ]
        );
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
        Some("io.github.underhilllabs.MarkdSmith"),
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

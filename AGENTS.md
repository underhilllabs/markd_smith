# Repository Guidelines

## Project Structure & Module Organization

This is a small Rust GTK/libadwaita desktop app. The crate manifest is
`Cargo.toml`, with locked dependency versions in `Cargo.lock`. Application code
currently lives in `src/main.rs` as a single binary that builds the editor UI,
menu system, Markdown conversion, and WebKit live preview. Development notes are
in `markd_smith-plan.md`. Build artifacts are
generated under `target/` and should not be edited or committed.

## Build, Test, and Development Commands

- `cargo run` starts the Markdown editor with the default welcome document.
- `cargo run -- path/to/file.md` opens an existing Markdown file at startup.
- `cargo build` compiles the debug binary.
- `cargo build --release` compiles an optimized release binary.
- `cargo fmt` formats Rust source using rustfmt.
- `cargo clippy -- -D warnings` runs lint checks and treats warnings as errors.
- `cargo test` runs the test suite. There are no tests yet, but use this command
  as tests are added.

The app depends on system GTK libraries: GTK4, libadwaita, GtkSourceView 5, and
WebKitGTK 6. `.cargo/config.toml` sets a WebKit sandbox environment variable for
local development.

## Coding Style & Naming Conventions

Use Rust 2021 idioms and rustfmt defaults. Keep four-space indentation and
prefer descriptive snake_case names for functions, variables, and modules. Use
PascalCase for types and enums, and SCREAMING_SNAKE_CASE for constants. Follow
the existing gtk-rs style: import prelude traits explicitly, keep widget setup
grouped by UI area, and avoid broad rewrites unless extracting a focused module.
For menu commands, prefer GTK/GIO actions with clear names such as `app.open` or
`win.toggle-preview`, and keep labels, accelerators, and handlers grouped near
the menu setup.

## Testing Guidelines

Prefer small unit tests for pure logic such as Markdown-to-HTML conversion.
Place inline tests in the relevant module with `#[cfg(test)]`, or add integration
tests under `tests/` when behavior crosses module boundaries. Name tests after
the behavior being verified, for example
`renders_tables_when_markdown_contains_table`. Run `cargo test` before opening a
pull request.

## Commit & Pull Request Guidelines

This repository has no existing commits, so use a clear conventional style such
as `feat: add file loading` or `fix: handle unreadable markdown files`. Keep
commits focused and explain user-visible changes in the body when needed.

Pull requests should include a short summary, testing performed, and screenshots
or screen recordings for UI changes. Link related issues when available and call
out any new system dependency or runtime configuration requirement.

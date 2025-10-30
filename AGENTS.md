# Repository Guidelines

## Project Structure & Module Organization
The crate root lives in `src/lib.rs`, which re-exports the public modules. HTTP clients for Bitbank live in `src/bitbank_public.rs` and `src/bitbank_private.rs`, while the actor-style runtime is in `src/bitbank_bot.rs`. Shared domain models are split between `src/bitbank_structs.rs` and the `src/bitbank_structs/` directory. Higher-level orchestration is handled by `src/order_manager.rs`, `src/response_handler.rs`, and `src/websocket_handler.rs`. Examples demonstrating end-to-end usage are under `examples/`, and integration artifacts produced by `cargo` land in `target/` (avoid committing anything there).

## Build, Test, and Development Commands
Use `cargo build` for a debug build and `cargo build --release` before publishing binaries. Run `cargo test` to execute the inline unit tests defined with `#[cfg(test)]`. Lint the code with `cargo clippy --all-targets --all-features` and format it via `cargo fmt --all`. To experiment with bots locally, run an example such as `cargo run --example best_mm mona_jpy 0.001 8000 0.001 0.002`.

## Coding Style & Naming Conventions
Follow Rust 2021 idioms with four-space indentation and `rustfmt` defaults; run `cargo fmt --all` before committing. Keep modules and files in `snake_case`, types and traits in `CamelCase`, and constants in `SCREAMING_SNAKE_CASE`. Prefer error propagation with the `?` operator and favor explicit `async fn` return types (e.g., `Pin<BoxFuture<_>>`) for clarity, matching the existing modules. Group related impls and keep public APIs documented with `///` comments when exporting new types.

## Testing Guidelines
Unit tests sit beside the code they verify (see the `#[cfg(test)]` blocks in the client modules). Add targeted tests for new API calls or serialization structs and cover both happy-path and failure responses. Run `cargo test` locally before opening a PR; if behavior depends on environment variables or network calls, gate those checks behind `#[ignore]` and document how to enable them.

## Commit & Pull Request Guidelines
Recent commits use short, imperative descriptions such as `delete unneeded files` and reference PRs when relevant (e.g., `refactoring ... (#10)`). Follow that pattern: begin with a lowercase verb phrase, keep the subject under 72 characters, and elaborate in the body when necessary. When opening a PR, reference the associated issue, describe the motivation and testing, and include screenshots or logs for behavioral changes. Make sure the branch is rebased on current `main` and that `cargo fmt`, `cargo clippy`, and `cargo test` all pass.

## Security & Configuration Tips
Private API features require `BITBANK_API_KEY` and `BITBANK_API_SECRET` environment variables; never hard-code or commit secrets. Prefer `.env` files ignored by git or your shell profile for local development. Double-check sample commands in `examples/` before sharing logs to avoid leaking credentials or trading strategies.

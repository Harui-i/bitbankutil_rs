# Repository Guidelines

## Project Structure & Module Organization
The library entry point is `src/lib.rs`, which re-exports clients defined in `src/bitbank_public.rs`, `src/bitbank_private.rs`, and bot helpers in `src/bitbank_bot.rs`. Shared DTOs live in `src/bitbank_structs` and `src/bitbank_structs.rs`. Execution flow helpers such as `order_manager.rs`, `response_handler.rs`, and `websocket_handler.rs` coordinate REST and WebSocket logic. Examples for bots and diagnostics reside in `examples/`; treat them as runnable references. Cargo build artifacts land in `target/`.

## Build, Test, and Development Commands
Run `cargo check` for a fast compile-time sanity pass. Use `cargo build --release` when producing binaries or benchmarking latency. Execute `cargo test` to run the module tests embedded in the client files. Format the code with `cargo fmt` and lint with `cargo clippy --all-targets --all-features` before opening a PR. To experiment with sample bots, run commands such as `cargo run --example best_mm mona_jpy 0.001 8000 0.001 0.002`.

## Coding Style & Naming Conventions
We target Rust 2021 with the default 4-space indentation enforced by rustfmt. Modules and directories use `snake_case`; public types stick to `CamelCase`; trait methods and functions remain `snake_case`. Keep API surface documentation up to date with `///` comments on public interfaces. Avoid introducing unwraps in library code—prefer `Result` propagation.

## Testing Guidelines
Unit tests live beside their modules under `#[cfg(test)]` blocks (see `bitbank_public.rs`). Mirror that pattern for new modules. Add integration tests under a new `tests/` directory when behaviour spans multiple components. Name tests after the scenario under test, e.g., `handles_invalid_signature`. Use `cargo test -- --nocapture` when debugging async output.

## Commit & Pull Request Guidelines
Commits in this repository use short, present-tense summaries (e.g., `implement default websocket reconnect`). Group related changes per commit and update `Cargo.toml` version numbers only when you intend to publish. Pull requests should describe the user-visible impact, list new commands or env vars, and link to relevant issues. Include instructions for validating changes (`cargo test`, bot runs) and attach screenshots or logs when touching runtime behaviour.

## Security & Configuration Tips
Private-client examples expect `BITBANK_API_KEY` and `BITBANK_API_SECRET` in your environment. Prefer local `.env` files loaded via your shell, and never commit credentials. Review dependencies with `cargo tree -d` when adding new crates, and enable API rate limiting when integrating production bots.

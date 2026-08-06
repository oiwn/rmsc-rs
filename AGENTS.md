# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 Cargo workspace separates reusable code from plugin wrappers:

- `crates/musictools-core/` contains framework-independent, suite-wide utilities.
- `crates/bogdan-dsp/` contains Bogdan's DSP, unit tests, and offline examples.
- `plugins/bogdan/` contains the Truce wrapper and egui editor.
- `specs/` records architecture, active work, roadmap items, and ideas.
- `truce.toml` defines plugin metadata; generated bundles belong in
  `target/bundles/` and must not be committed.

Keep plugin-specific signal processing in its DSP crate. Move code into
`musictools-core` only when multiple plugins can use it.

## Build, Test, and Development Commands

- `cargo build --workspace` compiles every workspace member.
- `cargo test --workspace --all-features` runs the complete unit-test suite.
- `cargo fmt --all --check` verifies rustfmt formatting.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` treats
  all lint warnings as failures.
- `cargo run -p bogdan-dsp --example triangle_probe` runs a DSP probe locally.
- `cargo truce build -p bogdan --clap --vst3` creates CLAP and VST3 bundles.
  Install `cargo-truce` version 6.3.0 first.

The hooks in `prek.toml` also run formatting, Clippy, tests, typo checks, and
secret scanning.

## Coding Style & Naming Conventions

Use standard rustfmt output (four-space indentation). Follow Rust conventions:
`snake_case` for modules, functions, and tests; `CamelCase` for types; and
`SCREAMING_SNAKE_CASE` for constants. Document public APIs and keep audio-thread
code allocation-free and robust against non-finite host input. Unsafe Rust is
forbidden workspace-wide.

## Testing Guidelines

Place focused `#[cfg(test)]` modules beside the code they exercise. Name tests
after observable behavior, such as `non_finite_values_use_fallback`. DSP changes
should cover ceiling bounds, finite output, channel independence, and state
reset behavior where relevant. Run the full test and Clippy commands before
opening a PR; there is no numeric coverage threshold.

## Commit & Pull Request Guidelines

Existing history uses short, informal subjects such as `initial design` and
`first version`. For new work, prefer concise imperative subjects, optionally
scoped, for example `bogdan-dsp: clamp fold output`. PRs should explain the
audible or architectural effect, list verification commands, and link relevant
issues or specs. Include screenshots for editor changes and listening or host
validation notes for audio behavior changes.

<!-- BEGIN specdev -->
## specdev

This project uses **specdev** (specification-driven development). Load the
specdev skill when starting a session, continuing from specs, or picking up a
task. Always read `specs/overview.md` and `specs/ctx.md` before coding.
<!-- END specdev -->

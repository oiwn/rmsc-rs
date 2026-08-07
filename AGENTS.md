# Repository Guidelines

## Project Structure & Module Organization

This Rust 2024 Cargo workspace separates reusable code from plugin wrappers:

- `crates/musictools-core/` contains framework-independent, suite-wide utilities.
- `crates/bogdan-dsp/` contains Bogdan's DSP, unit tests, and offline examples.
- `crates/kirya-dsp/` contains Kirya's plate reverb, its offline impulse-response
  analysis (feature `analysis`, on by default), unit tests, and examples.
- `plugins/bogdan/` and `plugins/kirya/` contain the Truce wrappers and egui
  editors.
- `specs/` records architecture, per-plugin specs, active work, roadmap items,
  and ideas. `specs/overview.md` holds workspace-wide architecture and house
  rules; `specs/bogdan.md` and `specs/kirya.md` own each plugin's description,
  signal chain, parameter reference, gotchas, and reference links. A new plugin
  gets its own `specs/<name>.md`.
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
- `cargo run -p kirya-dsp --release --example ir_probe` prints Kirya's impulse
  response, its reverb time, and an ASCII spectrogram.
- `cargo run -p kirya-dsp --release --example tank_stability` sweeps the reverb
  tank's parameter corners at four sample rates and fails on any runaway.
  Both Kirya examples want `--release`; they render tens of seconds of audio.
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
task. Always read `specs/overview.md` and `specs/ctx.md` before coding, plus the
`specs/<plugin>.md` for whichever plugin you are touching.
<!-- END specdev -->

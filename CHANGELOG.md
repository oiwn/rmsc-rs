# Changelog

Completed tasks, moved here from `specs/ctx.md` once every checkbox in their
plan is done. Newest entry at the top, dated. Freeform — one block per finished
task. Coexists with any conventional release notes already here.

<!-- Entry template:
## YYYY-MM-DD — <task title>

- what shipped
- files touched / decisions locked
-->

## 2026-08-04 — v0.2.0: modes + wavefolder

- Version bumped `0.1.0` → `0.2.0` (workspace root `Cargo.toml`); the editor
  header now shows a `v<version>` build stamp so the loaded bundle is
  identifiable (reinstall with `cargo truce install -p bogdan --clap --vst3
  --user` to refresh the host's copy).
- Added a **Mode** switch — Clean / Detail / Fold:
  - Clean: plain hard clip.
  - Detail: original 1 kHz high-passed-delta rectified duck (DnB-bass voice),
    kept bit-identical via the `process_sample` wrapper.
  - Fold: real antialiased **wavefolder** (first-order ADAA), transparent below
    the ceiling and reflecting above it; **Sine** or **Triangle** shape.
- New params: `Mode` (2), `Detail` Hz cutoff (3), `Amount` depth (4), `Shape`
  (5). Drive sets fold density, Ceiling the amplitude. Fixed the `Amount` `%`
  display (`linear(0,1)`).
- `bogdan-dsp`: `ClipMode`/`FoldShape`/`DetailSettings`, runtime cutoff with a
  cached coefficient, ADAA fold helpers; `finite_or_f64` + `safe_sample_rate`.
- Editor: larger/bolder title, centered control row with margins, Mode/Shape
  dropdowns.
- Verified: `cargo test --all` green, clippy clean, rt-paranoid 0 allocations,
  CLAP + VST3 build.

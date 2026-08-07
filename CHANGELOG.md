# Changelog

Completed tasks, moved here from `specs/ctx.md` once every checkbox in their
plan is done. Newest entry at the top, dated. Freeform — one block per finished
task. Coexists with any conventional release notes already here.

<!-- Entry template:
## YYYY-MM-DD — <task title>

- what shipped
- files touched / decisions locked
-->

## 2026-08-07 — Kirya follow-ups: IR window, Hz formatting, per-plugin specs

- **Adaptive IR probe window.** The probe rendered a fixed 3 s, so at the long
  decays this plugin exists for the tail never reached the floor and the
  readout degraded to "RT60 > 3 s". The window is now sized from the reverb's
  own tail estimate and snapped to one of `IR_WINDOW_STOPS` (0.5 s to 32 s,
  stepping ~1.6x) with 1.3x headroom. Extracted
  `kirya_dsp::estimated_tail_seconds` so the host's `tail()`, the render task
  and the `ir_probe` example all share one estimate. The editor's time axis is
  now data-driven, with tick spacing chosen per window. Visual only — no DSP
  change. Default settings now measure RT60 5.10 s where the truncated 3 s
  window reported 4.64 s; the dark preset reports 10.21 s instead of nothing.
- **Bounded analysis resolution**, required by the above: analysis hops widen on
  a long render instead of the point count growing without bound. At a fixed
  256-sample hop a 32 s render at 192 kHz would have needed ~24 000 spectrogram
  columns — past the 8192 texture limit common on GPUs, and 24 000 transforms
  per render. `IrRender` now carries the hops it chose. A 3 s / 48 kHz render is
  unchanged.
- **`musictools_core::format_hz`** — Truce formats `ParamUnit::Hz` as whole
  hertz below 1 kHz, which collapsed Mod Rate's whole 0.01–20 Hz range into
  "0 Hz" and "1 Hz". One shared formatter (2 decimals below 10 Hz, 1 below 100,
  integer below 1 kHz, then kHz with 2 decimals) is now wired via
  `#[param(format = ...)]` into Kirya's five Hz params and Bogdan's Detail, so
  both plugins read consistently. Display only; no ids, ranges or defaults
  changed, and automation is untouched.
- **Per-plugin specs.** New `specs/bogdan.md` and `specs/kirya.md` own each
  plugin's description, signal chain, parameter table, gotchas and reference
  links. `specs/overview.md` trimmed to workspace architecture, the crate map,
  and the house rules that apply everywhere. `AGENTS.md` and `README.md` point
  at the new layout.
- Verified: `cargo test --workspace --all-features` green (112 tests), clippy
  clean at `-D warnings`, `rt-paranoid` still 0 allocations. Both plugins
  rebuilt and reinstalled, since the formatter change touches Bogdan too.
  Listening and in-host checks are still outstanding.

## 2026-08-07 — Kirya: Dattorro plate reverb (CLAP + VST3)

- Second plugin in the suite. Clean-room implementation of the reverberator in
  Dattorro 1997 (JAES 45(9)) with ValleyAudio Plateau's control set. Plateau is
  GPL-3.0-or-later; nothing was copied, and `kirya-dsp` stays MIT.
- `crates/kirya-dsp` (new): `constants.rs`, `delay.rs` (`InterpDelay`),
  `allpass.rs`, `filters.rs`, `lfo.rs`, `tank.rs`, `analysis.rs` (feature
  `analysis`, default on, via `realfft`), and `KiryaReverb` / `KiryaSettings` /
  `KiryaFrame` in `lib.rs`. Examples `ir_probe` and `tank_stability`.
- `plugins/kirya` (new): 14 params (ids 0–13, a permanent contract), the
  `RenderIr` background task, and an egui editor with a stacked envelope +
  spectrogram IR probe over three control rows.
- Decisions locked during implementation:
  - **Mod Shape** morphs the tank LFOs from triangle (0%) to sine (100%).
  - The four cutoff params use `log(20)` smoothing rather than `exp(20)`;
    a frequency reads as smooth when it ramps by a constant ratio.
  - LFO phase is carried in `f64`. At the 0.01 Hz end of the rate knob one
    turn is ~4.8 M samples, where an `f32` step is a fraction of an ulp and the
    modulation would quantise into a staircase.
  - `OUTPUT_TRIM = 1.12`, measured: the untrimmed seven-tap sum sat at 0.893x
    the dry RMS at default settings. Wet at 100% now lands within 0.01 dB of
    dry.
  - Host tail time is estimated from the loop length and clamped to 30 s
    (`u32::MAX` while frozen); the raw figure runs to hours at decay 0.9999.
  - RT60 reports nothing rather than a guess when the tail has not fallen
    35 dB inside the render window — otherwise the end of the window, not the
    reverb, sets the slope.
  - **Three** `#[skip]` handoff fields, not the two originally planned:
    `sample_rate_bits: Arc<AtomicU64>` was added because `PluginContext` does
    not carry the host rate, and the IR probe has to render at whatever the
    host is playing or the displayed reverb time is wrong anywhere but 48 kHz.
  - `RenderIr` builds its `Analyzer` per run rather than sharing a scratch
    buffer. `SERIALIZED = true` is kept anyway for a different reason than the
    plan gave: it stops two concurrent renders finishing out of order and
    leaving a stale tail on screen.
  - Editor is 720x700, not the 720x620 planned — the two analysis panels plus
    three control rows do not fit in 620.
- Fixed while building: the tank's damping filters were not cleared by
  `reset`, so a reused instance injected a decaying tail into an empty tank.
- Verified: `cargo test --workspace --all-features` green (77 tests), clippy
  clean at `-D warnings`, `rt-paranoid` 0 allocations across 400 blocks while
  sweeping Size and toggling Freeze, `tank_stability` stable at 44.1/48/96/192
  kHz across every Size / Decay / Diffusion corner, CLAP + VST3 bundles built
  and signed.

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

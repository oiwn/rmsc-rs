# Task: Bogdan clip modes + tweak params

State: implemented; pending live host check

## Product
Bogdan — detail-preserving clipper (vendor `oiwn`), CLAP + VST3 only.
Target: drums & mix buses, esp. DnB bass.

## Done (v1)
- Pipeline: per-channel drive → hard clip at ceiling → delta = driven-clipped →
  1 kHz one-pole HP of delta → rectified inward duck (`clipped.abs()-|HP|`),
  zero latency, no oversampling. Sounds good on bass.
- 520×320 truce-egui scope: wait-free AudioTap streams [driven, processed],
  8192-frame history, triggered 1024-sample view, Drive + Ceiling knobs.
  Tests/clippy/rt-paranoid pass; CLAP verified live; VST3 load unconfirmed.

## Problem
Probe confirmed: on slow/triangle material the 1 kHz HP + abs() rectification
gives edge-notch artifacts (near-constant duck + spike back to ceiling at the
apex), not an inward image of the clipped peak. Current sound must be kept.

## Decision
Add a Mode switch — Clean / Detail / Fold — plus shared Detail (HP cutoff,
log 20–2000 Hz) and Amount (0–100%) params. Detail + 1 kHz + 100% == today,
bit-for-bit.
- Clean: plain hard clip.
- Detail: current rectified duck (DnB bass voice). Default.
- Fold: signed inward image — reduction = amount*HP*sign(clipped), clamped to
  [0, ceiling]; deepest at apex, DC-blocked so sustained clips relax to ceiling;
  pair with a low Detail cutoff.

## Implemented
- `bogdan-dsp`: `ClipMode {Clean,Detail,Fold}` + `DetailSettings`; runtime cutoff
  (`OnePoleHighPass::set_cutoff`, cached to skip `exp()` when static); `process()`
  with Fold = signed inward image; `process_sample` wrapper keeps Detail parity.
  `finite_or_f64` + `safe_sample_rate` added. 16 DSP tests pass.
- Wrapper: `Mode` EnumParam (id 2, default Detail) + Detail (id 3, log 20–2000 Hz,
  1 kHz) + Amount (id 4, 0–100%). Read once/frame into `DetailSettings`.
- Editor: `param_dropdown` Mode + Detail/Amount knobs in the control row.
- Verified: clippy clean, `--all` tests green, rt-paranoid 0 allocs, CLAP + VST3
  build. Probe confirms Fold keeps plateau edges at the ceiling (no notch);
  Detail unchanged. `examples/triangle_probe.rs` compares Detail vs Fold.

## Open
- Live host check: switch modes, confirm Detail == old sound, Fold peak-image,
  Clean flat; still confirm VST3 host load.
- Per-mode default cutoff (Fold ~30 Hz vs Detail 1 kHz) — one shared default for now.

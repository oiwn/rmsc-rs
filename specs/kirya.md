# Kirya — Dattorro plate reverb

A lush plate for turning arpeggios into pads. Clean-room implementation of the
reverberator in Jon Dattorro's "Effect Design Part 1" (JAES 45(9), 1997),
wearing the user-facing control set popularised by ValleyAudio's Plateau, plus
an impulse-response probe drawn live in the editor.

Crates: `crates/kirya-dsp` (DSP and offline analysis, MIT) and `plugins/kirya`
(Truce wrapper, background render task, egui editor). Formats: CLAP and VST3.

## How it works

### Signal flow

```
in L,R -> x0.5 sum -> DC block -> in low cut -> in high cut -> pre-delay
       -> 4 input diffusers -> split -> tank -> 7-tap network -> DC block
```

Lengths are quoted at the paper's reference rate `FS_REF = 29761` and scaled at
runtime by `sample_rate / FS_REF * size`. The input diffusers (lengths
`[142, 107, 379, 277]`, gains `[0.75, 0.75, 0.625, 0.625]`) and the pre-delay
scale with sample rate but **not** with Size: they set the initial echo
density, which should not stretch when the plate grows.

### The tank

A figure-of-eight. Two halves, named by their first allpass length, each
running `APF1 -> Delay 1 -> damping -> x decay -> APF2 -> Delay 2 -> x decay`,
with each half's output crossing into the *other* half's first allpass. The
diffused input is summed into both first allpasses.

| Element | Half A | Half B |
|---|---|---|
| APF1 (modulated), gain `-dd1` | 672 | 908 |
| Delay 1 | 4453 | 4217 |
| Damping: high cut LPF then low cut HPF | | |
| `x decay` | | |
| APF2, gain `+dd2` | 1800 | 2656 |
| Delay 2 | 3720 | 3163 |
| `x decay` → cross into the *other* half's APF1 | | |

Allpasses are the two-multiplier lattice with both coefficients identical:
`w = x - g*d_out; y = g*w + d_out; d_in = w`, giving
`H(z) = (g + z^-m)/(1 + g z^-m)` — unit magnitude at every frequency. APF1 takes
the coefficient with the **opposite sign** to APF2 (the paper's "note sign"
annotation on Fig. 1); relative polarity is what matters, not which is negative.

Both crossings are read from the past *before* either half writes, so the
figure-of-eight adds no delay beyond the paper's own lines.

### The output network

```
yL = 0.6*( +B.del1[266] +B.del1[2974] -B.apf2[1913] +B.del2[1996]
           -A.del1[1990] -A.apf2[187]  -A.del2[1066] )
yR = 0.6*( +A.del1[353] +A.del1[3627] -A.apf2[1228] +A.del2[2673]
           -B.del1[2111] -B.apf2[335]  -B.del2[121]  )
```

The plate is **mono**. Its stereo image comes entirely from this crossing — `yL`
is built mainly from half B and `yR` mainly from half A — not from two
independent tanks. At 48 kHz and Size 1.0 the earliest left tap lands near
sample 429 and the earliest right near 569; that offset is the image.

### Parameter mapping

- `dd1 = diffusion * 0.70`
- `dd2 = diffusion * clamp(decay + 0.15, 0.25, 0.50)` — the paper's stability
  clamp is on decay diffusion **2**, not 1. Scaling by Diffusion on top still
  lets one knob remove all tank diffusion.
- Decay knob maps as `d = 1 - (1-x)^2`, clamped to `0.1..0.9999`. Most of the
  travel lands in the long-tail region where small gain changes are audible.
- Four LFOs at `rate * [1.0, 1.37, 1.62, 1.93]`, phases `[0, 0.25, 0.5, 0.75]`,
  driving APF1/APF2 of both halves. Bipolar excursion `depth * 16 * sr_scale`
  samples on top of the size-scaled base length.
- **Mod Shape** morphs the LFOs from a triangle at 0% to a sine at 100%. Both
  are bipolar in `[-1, 1]` and rise through zero at phase zero, so the morph
  never steps.
- Freeze: decay crossfades to exactly 1.0 and damping fades out, over ~50 ms.
  The input stays live so material can be layered into a frozen tank.

## Parameters

Ids are a permanent contract: a host stores automation against them, so an id
must never be reused or renumbered.

| id | Name | Range | Default | Unit | Smoothing |
|---|---|---|---|---|---|
| 0 | Dry | `linear(0, 1)` | 1.0 | % | exp(5) |
| 1 | Wet | `linear(0, 1)` | 0.5 | % | exp(5) |
| 2 | Pre-Delay | `skewed(0, 500, 0.5)` | 0 | ms | exp(50) |
| 3 | Size | `skewed(0.05, 4, 0.4)` | 1.0 | | exp(50) |
| 4 | Diffusion | `linear(0, 1)` | 1.0 | % | exp(5) |
| 5 | Decay | `linear(0, 1)` | 0.55 | % | exp(5) |
| 6 | In Low Cut | `log(20, 20000)` | 20 | Hz | log(20) |
| 7 | In High Cut | `log(20, 20000)` | 20000 | Hz | log(20) |
| 8 | Rev Low Cut | `log(20, 20000)` | 20 | Hz | log(20) |
| 9 | Rev High Cut | `log(20, 20000)` | 10000 | Hz | log(20) |
| 10 | Mod Rate | `log(0.01, 20)` | 1.0 | Hz | log(20) |
| 11 | Mod Depth | `linear(0, 1)` | 0.5 | % | exp(5) |
| 12 | Mod Shape | `linear(0, 1)` | 0.5 | % | exp(5) |
| 13 | Freeze | `BoolParam` | false | | — |

Pre-Delay and Size take the slow `exp(50)` glide because they retune delay
lengths: it avoids zipper noise and gives a musical tape-style pitch slide. The
four cutoffs and Mod Rate use **`log`** smoothing rather than `exp` — a
frequency reads as smooth when it ramps by a constant ratio, not a constant
delta — and display through `musictools_core::format_hz`.

## The IR probe

The editor renders the current settings' impulse response on the background task
pool and draws its decay envelope and log-frequency spectrogram above the
controls. Renders are wet-only (so the dry spike does not dominate) with Freeze
forced off and pre-delay included.

- **Window**: sized from the reverb's own tail estimate and snapped to one of
  `IR_WINDOW_STOPS` (0.5 s to 32 s, stepping ~1.6x), with 1.3x headroom so the
  tail visibly reaches the floor. A fixed window cannot serve this plugin — at
  Size 0.05 / Decay 0 the tail is gone in ~30 ms, and at the top of the Decay
  range it runs for hours. Snapping rather than tracking continuously keeps the
  axis stable while a knob moves.
- **Envelope**: block RMS in dB relative to the render's peak.
- **Spectrogram**: 1024-point Hann, magnitude in dB, remapped to 128 log-spaced
  rows over 20 Hz – 20 kHz, uploaded once per render as a texture.
- **RT60**: T30 from a backward-integrated (Schroeder) energy decay curve,
  doubled.
- Requests are debounced 150 ms and posted with `spawn_coalescing`, so a knob
  drag collapses to one render.

## Gotchas / knowledge

- **Clean-room, deliberately.** Plateau is GPL-3.0-or-later and this workspace
  is MIT, so `kirya-dsp` was written from the paper. Kirya does not sound
  identical to Plateau, which deviates from the paper in ways not reproduced
  here: it reuses one mirrored 7-tap set instead of two distinct ones, reads tap
  1913 from a line that is nominally 1800 long, drops the 0.6 tap gain, and
  hard-clamps its tank to 44.1 kHz (so its reverb time in seconds shrinks to
  0.46x at 96 kHz). Its four LFO phase offsets are dead code; ours are applied.
  Never paste or mechanically transliterate Plateau's `Dattorro.cpp` /
  `Dattorro.hpp`.
- **Tap indices confirm the node-to-line mapping.** Every one of the fourteen
  output taps lands strictly inside the line it reads, at every Size and sample
  rate. That self-consistency is what pins the mapping down, and it is asserted
  in both `constants.rs` and `tank.rs`. `InterpDelay::read` clamps rather than
  wrapping, so an oversized tap would silently become a comb filter instead of
  panicking.
- **Freeze is lossless apart from the interpolator.** At full freeze the loop
  gain is exactly 1.0 and damping is faded out, so the only loss left is the
  fractional-delay linear interpolation, which is a mild low-pass. That costs
  roughly 2.5 dB of RMS over five seconds at Size 1.0 — an RT60 near two minutes
  — as the tail slowly darkens. Verified by measuring at Size 0.62002, where
  every tank length lands on an integer at 48 kHz and the droop falls to 0.5 dB.
  The interpolation is not optional: the output taps need it for Size sweeps to
  stay smooth.
- **Buffers are sized once, in `reset`, for `SIZE_MAX` plus modulation
  headroom.** Changing Size at runtime only changes read distances. The
  wrapper's `rt-paranoid` test sweeps Size across its whole range and toggles
  Freeze while asserting zero allocations.
- **LFO phase is carried in `f64`.** At the 0.01 Hz end of the rate knob one
  turn is ~4.8 M samples at 48 kHz, where an `f32` step is a fraction of an ulp
  and the modulation would quantise into a staircase.
- **The background task must not call `read()`.** `FloatParam::read()` advances
  the smoother, which belongs to the audio thread. The IR render and the
  editor's change detection both go through `target_settings()`, which reads raw
  targets and advances nothing.
- **RT60 reports `None` rather than a guess.** A backward-integrated energy
  curve always reaches its floor at the end of the window, so a tail that has
  not fallen 35 dB inside the render — or one whose -35 dB point lands in the
  last 10% — has no reliable slope to fit. The editor prints `> N s` instead.
- **Analysis hops widen on a long render** rather than the point count growing
  without bound. At a fixed 256-sample hop a 32 s render at 192 kHz would need
  ~24 000 spectrogram columns, past the 8192 texture limit common on GPUs.
- `OUTPUT_TRIM = 1.12` is measured, not guessed: the untrimmed seven-tap sum sat
  at 0.893x the dry RMS at default settings, and wet at 100% now lands within
  0.01 dB of dry.
- Host tail time is the loop-length estimate clamped to 30 s, and `u32::MAX`
  while frozen. The raw figure runs to hours at decay 0.9999.

## Readings

- Dattorro, "Effect Design Part 1: Reverberator and Other Filters", JAES 45(9),
  1997: <https://ccrma.stanford.edu/~dattorro/EffectDesignPart1.pdf>
- ValleyAudio Plateau, the source of the control set (GPL-3.0-or-later —
  reference for *ideas* only, never for code):
  <https://github.com/ValleyAudio/ValleyRackFree>

## Offline probes

```sh
cargo run -p kirya-dsp --release --example ir_probe        # IR, RT60, spectrogram
cargo run -p kirya-dsp --release --example tank_stability  # stability sweep
```

`tank_stability` drives every Size / Decay / Diffusion corner at 44.1, 48, 96 and
192 kHz, plus continuous Size sweeps and Freeze toggling, and exits non-zero on
any runaway or non-finite output.

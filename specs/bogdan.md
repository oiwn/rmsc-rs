# Bogdan — detail-preserving clipper

A clipper for drums and mix buses that keeps a hard outer ceiling while putting
the detail a normal clipper would flatten back *inside* the plateau. Also
carries an antialiased wavefolder as a second voice.

Crates: `crates/bogdan-dsp` (DSP, MIT) and `plugins/bogdan` (Truce wrapper and
egui editor). Formats: CLAP and VST3.

## How it works

The reference idea is not whole-signal foldback distortion. A normal clipper
turns a loud transient into a flat plateau and throws away everything that was
happening up there. Bogdan keeps that information and folds it back inward, so
the waveform keeps a fixed outer ceiling with smaller detail moving inside the
region that would otherwise be flat.

```
in -> x drive -> hard clip to ceiling -> clipped
                        |
                        +-> delta = driven - clipped
                                     |
                                     +-> one-pole high-pass (Detail knob)
                                                |
                                                +-> rectify, duck inward
```

Per sample the processor produces a `DetailFrame`: `driven`, `clipped`,
`delta`, `filtered_delta` and the final `output`. The editor's oscilloscope
draws `driven` against `output`.

### Modes

| Mode | What it does |
|---|---|
| **Clean** | Plain hard clip. The clipping delta is discarded. The reference path. |
| **Detail** | The original voice. The delta is high-passed, rectified, and subtracted inward from the clipped plateau: `magnitude = (\|clipped\| - amount * \|filtered_delta\|).max(0)`, sign restored from `clipped`. Suits drum-and-bass material. |
| **Fold** | Antialiased wavefolder. Excursions past the ceiling reflect back inside it. Drive sets fold density, Ceiling sets fold amplitude, Amount blends clip toward fold. |

### The wavefolder

`Fold` is a memoryless nonlinearity made alias-suppressed with **first-order
antiderivative antialiasing (ADAA)**:

```
y = (H(x) - H(x1)) / (x - x1)
```

where `H` is the antiderivative of the transfer function `h` and `x1` is the
previous driven sample. Below `ADAA_EPSILON` the quotient is ill-conditioned,
so it falls back to evaluating `h` at the midpoint.

Two shapes, both bounded to the ceiling `c` and both with closed-form
antiderivatives:

- **Sine**: `c * sin(pi x / 2c)`. Smooth and musical.
- **Triangle**: `c * (2/pi) * asin(sin(pi x / 2c))`. Brighter zig-zag folds;
  its antiderivative is built by phase reduction over the period `4c`.

The blend is `(1 - amount) * clip(x) + amount * fold(x)`, and its antiderivative
is the same blend of the two antiderivatives — so ADAA applies to the mix
directly rather than to each branch.

## Parameters

Ids are a permanent contract: a host stores automation against them, so an id
must never be reused or renumbered.

| id | Name | Range | Default | Unit | Smoothing |
|---|---|---|---|---|---|
| 0 | Drive | `linear(0, 24)` | 0 | dB | exp(5) |
| 1 | Ceiling | `linear(-24, 0)` | -0.1 | dB | exp(5) |
| 2 | Mode | Clean / Detail / Fold | Detail | | — |
| 3 | Detail | `log(20, 2000)` | 1000 | Hz | exp(20) |
| 4 | Amount | `linear(0, 1)` | 1 | % | exp(5) |
| 5 | Shape | Sine / Triangle | Sine | | — |

The Detail cutoff is displayed through `musictools_core::format_hz`, shared with
Kirya, so both plugins print frequencies the same way.

## Gotchas / knowledge

- **The output can never exceed the hard-clipping reference ceiling**, and
  unclipped samples always pass through unchanged. Both are asserted in tests
  across every mode and a range of ceilings.
- **The delta filter runs every sample**, including through unclipped gaps and
  across mode switches, so its memory stays warm and clipping resuming does not
  click.
- **`Detail` at Amount 0 is bit-identical to a plain hard clip**, which makes
  the knob a true depth control rather than a character change.
- The filter coefficient is recomputed only when the cutoff actually moves, so
  a static Detail knob costs no `exp()` per sample.
- Constant clipping settles back to the hard ceiling: the high-passed delta of a
  constant is zero, so a sustained plateau stops ducking. That is intended —
  the effect is about motion, not level.
- The ADAA quotient can nudge a hair past the ceiling numerically, so the result
  is clamped.

## Readings

- Au5 describes the reference method as high-passed foldback of the clipping
  delta, using ring-mod sidechain ducking to subtract detail from the clipped
  signal: <https://www.patreon.com/Au5Music/posts/detail-clipper-156412018>
- Kilohearts Compactor, the reference building block, because it can duck at
  sample rate: <https://kilohearts.com/products/compactor>
- Jatin Chowdhury on antialiased wavefolding:
  <https://ccrma.stanford.edu/~jatin/ComplexNonlinearities/Wavefolder.html>

## Status

The first usable implementation uses independent channels, a one-pole 1 kHz
high-pass on the clipping delta, unity inward subtraction, zero latency, and no
oversampling. These are conservative v1 defaults rather than a claim that the
measured and listened-to comparison is complete.

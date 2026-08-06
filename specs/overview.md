# Project Overview

## Architecture

The repository is a Truce-based Cargo workspace designed to host a suite of
audio plugins. Shared dependency versions live in the virtual workspace root.
`musictools-core` owns framework-independent suite utilities,
`bogdan-dsp` owns Bogdan-specific processing, and the `bogdan` plugin crate
contains only its Truce wrapper.

The suite targets CLAP and VST3 only. Standalone applications, Audio Units,
VST2, LV2, AAX, and other plugin formats are out of scope.

New plugins belong under `plugins/` and opt into the centrally pinned Truce
dependencies. Code moves into `musictools-core` only when it is genuinely
suite-wide; plugin-specific DSP remains in its own crate.

## Data Flow

Bogdan is intended to be a detail-preserving clipper. The reference topology
to validate is:

1. Drive the input into a hard ceiling.
2. Compute the delta between the driven and clipped signals.
3. High-pass that delta to isolate detail hidden by clipping.
4. Convert the filtered delta into sample-rate modulation.
5. Duck or subtract inward from the clipped plateau while preserving the hard
   outer ceiling.

The first usable implementation uses independent channels, a one-pole 1 kHz
high-pass on the clipping delta, unity inward subtraction, zero latency, and no
oversampling. These are conservative v1 defaults rather than a claim that the
later measured and listened-to comparison is complete.

## Gotchas / Knowledge

- Detail-preserving clipping is not whole-signal foldback distortion. The
  desired waveform retains a fixed outer ceiling with smaller inward detail
  inside regions a normal clipper would flatten.
- Au5 describes the reference method as high-passed foldback of the clipping
  delta, using ring-mod sidechain ducking to subtract detail from the clipped
  signal: <https://www.patreon.com/Au5Music/posts/detail-clipper-156412018>.
- Kilohearts Compactor is the reference building block because it can duck at
  sample rate: <https://kilohearts.com/products/compactor>.
- The primary target material is drums and mix buses, not transparent
  true-peak mastering or unrestricted creative wavefolding.

## Readings

- <https://ccrma.stanford.edu/~jatin/ComplexNonlinearities/Wavefolder.html>

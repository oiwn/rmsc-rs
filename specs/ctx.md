# Current Task Context: Specify Bogdan detail-preserving clipper

State: in progress

## Plan

- [x] Add a wait-free audio-to-GUI scope feed for the driven and processed
  signals without allocating or locking in `process`.
- [x] Replace the undersized stock grid with a fixed-size custom editor around
  520×320 logical pixels.
- [x] Draw a stable triggered oscilloscope with driven input and processed
  output overlays plus visible positive and negative ceiling guides.
- [x] Retain the Drive and Ceiling controls in a clear layout beneath the scope.
- [ ] Verify GUI behavior, real-time safety, and host loading in both CLAP and
  VST3 builds.
- [ ] Compare candidate delta extraction, filtering, rectification, scaling,
  stereo, and ceiling behavior.
- [ ] Validate the candidates against synthetic signals, drum/bus material,
  spectra, and the supplied waveform reference.
- [ ] Select and document the final real-time pipeline, parameters, latency,
  and antialiasing strategy.
- [ ] Produce decision-complete implementation and acceptance criteria for the
  Truce plugin.

## Findings

- Product: **Bogdan — Detail-Preserving Clipper**, vendor `oiwn`.
- The desired result has a hard outer ceiling but retains inward variations
  within clipped peaks.
- This is neither ordinary whole-signal foldback nor transient detection.
- Reference hypothesis: hard-clip the driven signal, derive and high-pass the
  lost clipping delta, then use its magnitude to duck inward from the clipped
  plateau at sample rate.
- Primary material is drums and mix buses.
- Exact normalization, filtering, polarity, stereo linking, latency, and
  antialiasing must be measured rather than assumed.
- The current zero-latency v1 uses independent channels, a one-pole 1 kHz
  high-pass, unity inward subtraction, no oversampling, and the existing Drive
  and Ceiling parameters.
- Distribution scope is CLAP and VST3 only. Standalone, Audio Unit, VST2, LV2,
  AAX, and other plugin targets are out of scope.
- The stock Truce `GridLayout` renders at roughly 140×120 pixels and cannot
  host custom oscilloscope drawing. Truce provides a wait-free `AudioTap` for
  streaming samples from the audio thread to a custom editor.
- The implemented 520×320 `truce-egui` editor drains an interleaved
  driven/processed `AudioTap` into an 8,192-frame history and renders a
  triggered 1,024-sample view with Drive and Ceiling controls.
- Unit, integration, strict Clippy, and `rt-paranoid` checks pass; the CLAP and
  VST3 release bundles both build, and a live CLAP test confirmed the scope and
  detail-preserving waveform on bass material. VST3 host loading remains to be
  confirmed.

## Context

The product remains a detail-preserving clipper with this signal flow:

1. Apply input drive.
2. Hard-clip at the selected ceiling.
3. Compute the clipping delta between driven and clipped signals.
4. High-pass the delta to isolate detail that would otherwise disappear.
5. Rectify or otherwise derive a sample-rate modulation signal.
6. Subtract or duck that detail inward from the clipped waveform without
   exceeding the ceiling.

The scope should make that behavior visible rather than act as decoration. Tap
channel 0 (or the mono channel) after drive and after detail-preserving clipping,
transfer block-sized interleaved frames through Truce `AudioTap`, and drain on
the GUI thread at approximately 60 FPS. Keep a recent 1,024-sample view, trigger
on a rising zero crossing of the driven signal, and draw both signals with
distinct colors. Preallocate the block transfer buffer during `reset`; the
audio callback must remain free of allocation, locks, and I/O.

Use a custom Truce editor backend because the built-in grid supports parameter
widgets and meters but not arbitrary scope rendering. Keep the first version
fixed-size and focused: scope on top, Drive and Ceiling below, no additional
parameters or format targets.

## Next

Load the built VST3 in a host and confirm its editor/audio behavior, then begin
the candidate DSP comparison.

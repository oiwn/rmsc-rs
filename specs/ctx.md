# Current Task Context: Igorek two-stage convolver
State: in progress
## Plan
- [x] `crates/igorek-dsp` skeleton: workspace member, `IgorekSettings`/frame
      structs, `safe_sample_rate`, identity default Color IR
- [x] Partitioned overlap-save engine (P=128, N=256, block carry) + tests vs
      naive direct convolution
- [x] Room generator port from stoRIR-rs with stereo decorrelation + tests
      (RT60 target, ITDG placement, seed determinism, L/R independence)
- [x] Selection + 3-point envelope baking, RMS-normalized WAV load path
      (`hound`, linear resample, caps, non-finite rejection)
- [x] Processor assembly: stage mixes, Dry/Wet, install/swap protocol
- [x] `plugins/igorek` wrapper: 22 params (ids per spec), `latency()=128`,
      custom-state persistence of the Color IR, `rt-paranoid` test
- [x] Background build task (debounced, `spawn_coalescing`) + editor: IR
      panes, envelope/selection dragging, `rfd` load button on helper thread
- [x] `truce.toml` entry (fourcc `Igrk`), CLAP+VST3 bundles, fmt/clippy/tests
      green
- [x] Specs + CHANGELOG updated
## Findings
- **OPEN — Load WAV dialog still fails in the user's host** even after the
  switch to `rfd::AsyncFileDialog` (sheet modal, polled per frame). Deferred
  until testing in Waveform. Suspects: rfd's async path falls back to the
  crashing sync `runModal` when `NSApp` is not running or no window is
  found (hosts that are not full AppKit citizens), and/or attaching the
  sheet from inside baseview's draw callback. Next step: reproduce in a
  local host and watch for rfd's "fallback to sync" console message.
- Supply chain verified clean: `arrayref` pinned at 0.3.9 in `Cargo.lock`
  (compromised claim was 0.3.10, which does not exist on crates.io); no
  `proc-macro1` / `append-only-vec` / `internment` anywhere; `cargo audit`
  (249 deps) reports zero advisories. `cargo-audit` now installed.
- truce-egui 6.3 cannot do drag'n'drop: baseview-truce `WindowEvent` is only
  Resized/Focused/Unfocused/WillClose, so egui `dropped_files` never fires.
  Use `rfd` — but its *sync* `pick_file` crashes hosts (nested `NSApp` run
  loop inside baseview's draw callback); `AsyncFileDialog` (sheet modal,
  polled per frame) is the working path.
- `PluginLogic::latency() -> u32` exists — report 128 samples.
- `truce_core::custom_state` (`#[derive(State)]`, `StateBinding`, binary
  `StateField`s) exists — persist the loaded Color IR in host sessions.
- stoRIR-rs is the user's own sole-authored GPL repo; porting into the MIT
  workspace is permitted by the author. Provenance recorded in the spec.
- realfft (like rustfft) is unnormalized: a forward+inverse round trip gains
  N=256. The engine divides by N when copying its output block.
- On macOS a `std::sync::Mutex`'s first ever lock allocates its pthread
  storage (lazy `OnceBox` in std). A never-published swap slot would have
  allocated on the audio thread at the first block; `StageSwap::warm_up`
  in `reset` pays that cost off-thread.
- The ported stoRIR generator measures at ~0.6x its nominal RT60 (the
  squared gain map steepens the decay) — a known characteristic, guarded by
  regression windows in the room tests, not a port bug.
- truce's rt audit counts allocations always, frees only opt-in — the Box
  drop when installing new spectra on the audio thread passes the audit and
  is the documented cost of the wholesale swap.
## Context
Full design lives in [`igorek.md`](igorek.md): signal chain, the 22-parameter
id contract, engine/IR-swap protocol, gotchas (first block-based processor,
first audio-path FFT, IR caps and CPU budget), readings, offline probes.
Decisions from the user: Color IRs come from the user's own WAV files (no
synthesized library), ~2.7 ms latency is fine, envelope/selection are host
params, stoRIR code may be ported directly.
## Next
All checkboxes are done and the CLAP/VST3 bundles are built under
`target/bundles/`. What is left is human: load Igorek in a host, check the
editor (pane drags, Load button, room regeneration), and listen. Archiving
to CHANGELOG waits for an explicit close request.

# Igorek — two-stage convolver for percussion

A convolver built for drums and percussion. Stage one, **Color**, convolves with
a *material* IR (metal, wood, shell — the user's own WAV files) to preserve the
object's resonant properties. Stage two, **Room**, convolves with a
procedurally generated stochastic room IR. Each stage has a Bitwig-convolver
style **selection** (start + length) and **3-point envelope** baked into the IR
before the FFT, so shaping costs nothing at runtime.

Crates: `crates/igorek-dsp` (DSP, offline probes, MIT) and `plugins/igorek`
(Truce wrapper, background IR-build task, egui editor). Formats: CLAP and VST3.

## How it works

### Engine — uniformly partitioned overlap-save convolution

- Partition size **P = 128 samples** (not ms) at every sample rate: 2.7 ms at
  48 kHz, 2.9 ms at 44.1 kHz. FFT size N = 256 via `realfft` (129 complex bins).
- Per 128-sample block, per channel, per stage: one forward rFFT of the new
  input block into a circular frequency-domain delay line, P_ir complex
  spectrum multiplies, one inverse rFFT. Cost scales with IR length in
  partitions — see the budget note in Gotchas.
- Both stages run on the **same 128-sample grid inside one `process` call**
  (stage 2 consumes stage 1's block output within the same callback), so total
  latency is one partition: **128 samples**, reported through
  `PluginLogic::latency`.
- Host blocks are arbitrary; the processor carries a partial-block residue
  internally so `process` accepts any block size while the engine still steps
  in 128s. Latency stays 128 samples.
- This is the suite's first **block-based** processor and first **audio-path
  FFT** (`realfft` was analysis-only in kirya). The per-sample
  settings-in/frame-out pattern applies at block granularity instead:
  a `Copy` settings struct read once per block, a frame struct exposing
  intermediate signals for tests and the editor.

### IR install protocol (off-thread build → audio swap)

IR changes (file load, room regeneration, envelope/selection edits, sample-rate
change) are **baked off the audio thread**: trim, envelope, FFT, partition
spectra — all on a background task. The audio thread installs the result at a
block boundary via the kirya handoff pattern: `Arc<Mutex<Option<Box<Spectra>>>>`
plus an `AtomicU64` generation counter. `try_lock` only; if the lock is busy
the engine keeps the previous spectra for one more block. No allocation, no
blocking, no re-FFT on the audio thread, ever.

Buffers are sized in `reset` for the worst case (IR caps below), so a runtime
IR change only swaps pointers and a partition count.

### Color stage (material IR from the user's files)

- The user loads a WAV (mono or stereo) through a **native file dialog**
  (`rfd`). Drag'n'drop is not possible — see Gotchas.
- `hound` decodes off-thread; the file is resampled to the host rate (linear
  interpolation — the same mild HF rolloff kirya's fractional delays accept)
  and RMS-normalized to a fixed reference so arbitrary files sit at a
  predictable level relative to the Mix knob.
- Stereo files convolve diagonally: in L → IR L, in R → IR R. A full 2-in ×
  4-conv × 2-out matrix is a roadmap item, not v1.
- A non-finite or unparsable file is rejected with an editor notice; the
  previous IR stays loaded.
- Cap: **4 s**. Longer files load truncated, with an editor notice.
- Default IR before anything is loaded: the **identity impulse** (single
  unit sample) — the plugin passes audio uncolored on insert.
- The loaded IR persists in the host session via Truce `custom_state`
  (`#[derive(State)]` struct; samples as a binary `StateField`), so recall
  restores the sound without re-dropping the file.

### Room stage (generated, ported from stoRIR-rs)

A port of `oiwn`'s own `stoRIR-rs` (sole-authored; the GPL-3 repo's code is
embedded here with the author's permission — see Licensing under Gotchas).
Stochastic IR generation from:

- **RT60** — target reverberation time.
- **EDT** — early decay time.
- **ITDG** — initial time delay gap (the dead time before the first
  reflection).
- **ER Duration** — span of the early-reflection cluster.
- **Variant** — the seed selector. Integer semantics; one step is a fresh but
  deterministic room.

**Stereo by decorrelation:** the left IR is generated from
`(seed, params)`; the right from `(seed + round(width·997),
params jittered by width·small%)`. At Width 0 the offset and jitter vanish and
both channels are identical (mono room); at Width 1 they are fully decorrelated
stereo. Nothing is delayed or Haas-tricked between channels — the variation is
in the generated material itself.

The generated IR length follows RT60 with ~1.3× headroom, capped at **8 s**.
Regeneration runs on the background task, debounced 150 ms with
`spawn_coalescing`, exactly like kirya's IR probe.

**Port deviations from stoRIR-rs** (all deliberate, algorithm otherwise
faithful): one seeded `StdRng` per channel replaces `thread_rng` (the
determinism contract — the original's rooms are non-reproducible); plain
`Vec<f32>` instead of ndarray; `rt60 <= edt` clamps instead of panicking;
the RT60 decay ramp continues into the 1.3× headroom instead of stopping at
RT60; output is peak-normalized. DRR stays a fixed internal constant (-1.0)
— no parameter exposes it. A known characteristic carried over: the
generator's measured decay lands at roughly **0.6× the nominal RT60** (the
squared gain map steepens the slope); the room tests guard that ratio as a
regression window rather than pretending it is physical accuracy.

### Selection + 3-point envelope (per stage, baked)

- **Sel Start** + **Sel Length** choose the audible region of the IR; material
  outside is zeroed.
- The **3-point envelope** rides the selection: A at its start, B at a
  draggable interior x, C at its end. Y axis in dB (−60 … +12, 0 dB default =
  flat, i.e. untouched IR).
- Both are **host parameters** (ids below are a permanent contract) and are
  applied to the IR *before* the FFT during the background bake — zero runtime
  cost, and edits land at the next spectra swap rather than zipperingly.
  That is also why they take no smoothing: a smoothed value would be invisible
  between bakes.

### Signal flow

```
in L,R -> sanitize -> [Color: conv, Mix crossfade] -> [Room: conv, Mix crossfade]
      -> Dry/Wet -> out L,R
```

Defaults make Igorek a *replace-style* effect: Dry 0 / Wet 1, both stage mixes
100%, identity Color IR, a short default room.

## Parameters

Ids are a permanent contract: a host stores automation against them, so an id
must never be reused or renumbered. The exact range macro is an implementation
choice; the id, meaning, default, and unit are the contract.

| id | Name | Range | Default | Unit | Smoothing |
|---|---|---|---|---|---|
| 0 | Dry | 0…1 | 0.0 | % | exp(5) |
| 1 | Wet | 0…1 | 1.0 | % | exp(5) |
| 2 | Color Mix | 0…1 | 1.0 | % | exp(5) |
| 3 | Color Sel Start | 0…0.95 | 0 | % | — (baked) |
| 4 | Color Sel Length | 0.05…1 | 1 | % | — (baked) |
| 5 | Color Env A | −60…+12 | 0 | dB | — (baked) |
| 6 | Color Env B X | 0…1 | 0.5 | % | — (baked) |
| 7 | Color Env B | −60…+12 | 0 | dB | — (baked) |
| 8 | Color Env C | −60…+12 | 0 | dB | — (baked) |
| 9 | Room Mix | 0…1 | 1.0 | % | exp(5) |
| 10 | Room Sel Start | 0…0.95 | 0 | % | — (baked) |
| 11 | Room Sel Length | 0.05…1 | 1 | % | — (baked) |
| 12 | Room Env A | −60…+12 | 0 | dB | — (baked) |
| 13 | Room Env B X | 0…1 | 0.5 | % | — (baked) |
| 14 | Room Env B | −60…+12 | 0 | dB | — (baked) |
| 15 | Room Env C | −60…+12 | 0 | dB | — (baked) |
| 16 | Room RT60 | 0.05…8 | 0.8 | s | — (baked) |
| 17 | Room EDT | 5…1000 | 50 | ms | — (baked) |
| 18 | Room ITDG | 0…50 | 4 | ms | — (baked) |
| 19 | Room ER Duration | 5…500 | 100 | ms | — (baked) |
| 20 | Room Variant | 0…100 | 0 | (int) | — (baked) |
| 21 | Room Width | 0…1 | 1.0 | % | — (baked) |

The baked room parameters (16–21) take no smoothing because each step of a
smoother would be inaudible — the room only changes at the next debounced
regeneration. Mix and Dry/Wet are live crossfaders and glide with exp(5).

Envelope dB parameters may be stored internally as 0…1 with a dB-mapped
`format` callback if Truce's range macros dislike negative linear ranges; the
contract stays "the value in dB".

## Editor

- Two stacked IR panes (Color, Room): waveform of the *shaped* IR (post trim +
  envelope), selection region shaded, envelope polyline over it.
- Drag handles: selection start/end, and envelope points A/B/C (B's x drags
  too, inside the selection).
- A Load button for the Color WAV (native `rfd` dialog). The dialog blocks, so
  it opens on a helper thread; the editor polls the result on repaint, then
  hands the path to the background build task.
- Room pane rebuilds on any baked room param change, debounced 150 ms.
- No drag'n'drop — the Load button is the interface. See Gotchas.

## Gotchas / knowledge

- **Drag'n'drop is impossible in truce-egui 6.3.** The window events come from
  the `baseview-truce` fork, whose `WindowEvent` enum is only
  `Resized / Focused / Unfocused / WillClose` — there are no file-drop events,
  so egui's `dropped_files` can never populate. The native `rfd` dialog is the
  v1 interface; if baseview-truce ever grows file events, drag'n'drop becomes
  an easy follow-up (roadmap).
- **rfd's *sync* `FileDialog` crashes hosts from the editor.** `ui()` runs
  inside baseview's Metal draw callback; `pick_file()` spins a nested
  `NSApp` run loop (`NSOpenPanel.runModal`), which re-enters rendering from
  inside the draw callback and takes the host down. The fix is rfd's
  `AsyncFileDialog`: creating the future attaches a sheet via
  `beginSheetModal` with a completion block — non-nesting, safe to start
  inside `ui()` — and the editor polls the future each frame (it repaints
  every 33 ms anyway) with a no-op waker. Only the decode/resample runs on
  the helper thread.
- **First block-based processor in the suite.** The house per-sample rules
  (settings-in/frame-out, allocate only in `reset`) carry over at block
  granularity; `process` additionally owns the partial-block carry so hosts
  may call it with any block size.
- **First audio-path FFT.** `realfft` moves from kirya's analysis-only role
  onto the audio thread. Budget, per 128-sample block, per channel, per stage:
  one 256-pt rFFT, `n_partitions` 129-bin complex multiplies, one 256-pt
  inverse. At 48 kHz with a 1 s room (≈375 partitions, stereo, 2 stages) that
  is roughly 0.3 Gflop/s — comfortable; an 8 s room pushes toward 2.3 Gflop/s,
  which is the documented worst-case corner. Non-uniform (Gardner)
  partitioning is the roadmap remedy if that corner hurts.
- **IR caps bound memory, not sound:** Color 4 s, Room 8 s. Buffers sized in
  `reset` for the caps: worst case (8 s room at 192 kHz) is ~12 000 partitions
  × 129 bins × 8 B × 2 ch ≈ 24 MB of spectra. At 48 kHz the same cap is 6 MB.
- **The audio thread never sees a raw IR.** Everything it consumes is
  precomputed spectra arriving through the generation-counter swap. A
  half-installed state is impossible by construction: the swap is a single
  pointer replacement under a short mutex hold.
- **Identity impulse as the default Color IR** means "no file loaded" is not
  silence: with Dry 0 / Wet 1 the plugin still passes audio through the room.
- **Licensing.** `stoRIR-rs` is GPL-3, but it is the sole work of this
  workspace's author (`oiwn`, 21 commits, no other contributors), who has
  explicitly permitted embedding the generation code here as MIT. Record that
  provenance here; do not accept third-party patches into the ported generator
  without re-clearing this.
- **Sample-rate changes re-bake everything:** the room regenerates at the new
  rate, the Color WAV re-resamples, spectra rebuild. All off-thread; the audio
  thread runs the old spectra until the new generation installs.
- **Non-finite guard is total:** host samples through `finite_or`, and a
  decoded IR with NaN/inf is rejected before it can reach an FFT (rustfft
  would happily spread garbage across every bin).
- **The `rt-paranoid` test must sweep the swaps:** IR generation installs and
  partial-block carries are the allocation risks unique to this plugin; the
  zero-allocation audit drives both.
- **On macOS, a `std::sync::Mutex`'s first ever lock allocates its pthread
  storage** (lazy `OnceBox` inside std). A swap slot that has never been
  published to would pay that allocation on the audio thread at the first
  block. `StageSwap::warm_up` locks each slot once during `reset` so the
  audio thread's `try_lock` never can. Found by the rt-paranoid audit, not
  by reading code.
- **realfft is unnormalized** (it follows rustfft): a forward+inverse round
  trip multiplies by N. The engine applies 1/N when copying its output
  block, so spectra stay in natural FFT units — the identity IR's spectrum
  is all ones.
- **Latency accounting:** the overlap-save engine itself is zero-latency at
  the 128 grid (output block *j* completes with input block *j*); the
  plugin's exact 128-sample latency comes from the processor's output ring,
  which holds every block one partition and is drained *before* the incoming
  sample is staged. Dry rides the same ring as wet, so plugin latency is
  uniform and PDC-aligned.
- **Installing new spectra drops the previous `Box` on the audio thread.**
  truce's audit counts frees only opt-in (off by default), and the swap
  stays a single pointer replacement; the free is the documented cost of
  the wholesale-swap design.
- **The Color IR persists resampled at the rate active when it was saved**
  (`source_rate` stored alongside), and recall at a different rate
  re-resamples linearly from the stored samples — the original file is not
  kept.

## Readings

- stoRIR-rs, the room generator being ported (author's own repo):
  <https://github.com/oiwn/stoRIR-rs>. Original stoRIR (IoSR, University of
  Surrey): <https://github.com/IoSR-Surrey/stoRIR>
- Gardner, "Efficient Convolution without Input-Output Delay",
  JAES 43(3), 1995 — the partitioning scheme (we use its uniform special case).
- Wefers, *Partitioned Convolution Algorithms for Real-Time Auralization*
  (Logos, 2015) — the circular frequency-domain delay-line formulation.

## Offline probes

```sh
cargo run -p igorek-dsp --release --example conv_probe   # engine vs naive convolution
cargo run -p igorek-dsp --release --example ir_probe     # color/room IRs, ASCII view
```

`conv_probe` validates the partitioned engine against a naive direct
convolution across IR lengths that straddle partition boundaries (shorter than
P, exactly P, P+1, many partitions) and prints max deviation. `ir_probe`
renders the default room and a synthetic metallic IR with kirya-style envelope
and spectrogram views.

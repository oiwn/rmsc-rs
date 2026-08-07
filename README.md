# MusicTools

Rust audio plugins by `oiwn`, built as a multi-plugin workspace with
[Truce](https://github.com/truce-audio/truce).

**Bogdan** is an experimental detail-preserving clipper. Its zero-latency v1
high-passes the signal removed by hard clipping and uses that detail to
modulate clipped plateaus inward while retaining the sample ceiling.

**Kirya** is a Dattorro plate reverb: the 1997 paper's topology with Plateau's
control set — size, freeze, four modulated tank allpasses, split input and
reverb damping — plus an impulse-response probe in the editor that renders the
current settings off-thread and draws their decay envelope and log-frequency
spectrogram. Written clean-room from the paper; it is not a Plateau clone and
does not sound like one.

Each plugin has its own spec covering how it works, its parameters, and its
reference links: [`specs/bogdan.md`](specs/bogdan.md) and
[`specs/kirya.md`](specs/kirya.md). Workspace-wide architecture and house rules
live in [`specs/overview.md`](specs/overview.md); the active task is tracked in
[`specs/ctx.md`](specs/ctx.md).

## Workspace

- `crates/musictools-core` — suite-wide identity and reusable real-time-safe
  utilities.
- `crates/bogdan-dsp` — framework-independent clipper primitives and tests.
- `crates/kirya-dsp` — framework-independent plate reverb, offline
  impulse-response analysis, and tests.
- `plugins/bogdan`, `plugins/kirya` — Truce wrappers for the supported CLAP and
  VST3 formats.

CLAP and VST3 are the only supported plugin formats. Standalone applications,
Audio Units, VST2, LV2, AAX, and other formats are intentionally out of scope.

## Development

Install the matching Truce CLI and run the normal Rust checks:

```sh
cargo install cargo-truce --version 6.3.0
cargo fmt --all --check
cargo clippy --all --workspace
cargo test --all --workspace
```

Once `cargo-truce` is installed, build and install both formats for the current
macOS user:

```sh
cargo truce install -p bogdan --clap --vst3 --user
cargo truce install -p kirya --clap --vst3 --user
```

This installs into `~/Library/Audio/Plug-Ins/CLAP` and
`~/Library/Audio/Plug-Ins/VST3`. Restart the DAW or trigger its plugin rescan,
then load **Bogdan** or **Kirya** as an audio effect. To produce bundles under
`target/bundles/` without installing them, swap `install` for `build` and drop
`--user`.

Both DSP crates can be exercised without a host:

```sh
cargo run -p bogdan-dsp --example triangle_probe
cargo run -p kirya-dsp --release --example ir_probe
cargo run -p kirya-dsp --release --example tank_stability
```

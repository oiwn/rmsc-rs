# MusicTools

Rust audio plugins by `oiwn`, built as a multi-plugin workspace with
[Truce](https://github.com/truce-audio/truce).

The first plugin is **Bogdan**, an experimental detail-preserving clipper. Its
zero-latency v1 high-passes the signal removed by hard clipping and uses that
detail to modulate clipped plateaus inward while retaining the sample ceiling.
The broader DSP comparison remains tracked in [`specs/ctx.md`](specs/ctx.md).

## Workspace

- `crates/musictools-core` — suite-wide identity and reusable real-time-safe
  utilities.
- `crates/bogdan-dsp` — framework-independent DSP primitives and tests.
- `plugins/bogdan` — Truce wrapper for the supported CLAP and VST3 formats.

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
```

This installs Bogdan into `~/Library/Audio/Plug-Ins/CLAP` and
`~/Library/Audio/Plug-Ins/VST3`. Restart the DAW or trigger its plugin rescan,
then load **Bogdan** as an audio effect. To produce bundles under
`target/bundles/` without installing them, run
`cargo truce build -p bogdan --clap --vst3`.

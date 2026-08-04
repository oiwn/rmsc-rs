# Truce Framework Reference

General, project-agnostic notes on building plugins with **Truce `6.3.0`** (the
audio-plugin framework this workspace targets). Snippets are distilled from the
`truce`, `truce-params`, `truce-derive`, and `truce-egui` crate sources plus the
working plugin in this repo. Keep this file free of task-specific detail — it is
a cheat-sheet so we stop re-deriving the API each time.

## Crates & workspace wiring

Version-pin everything to the same Truce release in the workspace root
`Cargo.toml`:

```toml
truce      = { version = "=6.3.0" }
truce-egui = { version = "=6.3.0" }   # egui editor backend
truce-clap = { version = "=6.3.0" }   # CLAP wrapper
truce-vst3 = { version = "=6.3.0" }   # VST3 wrapper
```

A plugin crate opts into formats via features:

```toml
[features]
clap        = ["dep:truce-clap", "dep:clap-sys"]
vst3        = ["dep:truce-vst3"]
rt-paranoid = ["truce/rt-paranoid"]   # allocation auditing in the audio thread

[dependencies]
truce      = { workspace = true }
truce-egui = { workspace = true }
truce-clap = { workspace = true, optional = true }
truce-vst3 = { workspace = true, optional = true }
```

Everyday imports come from the prelude:

```rust
use truce::prelude::*;
```

The prelude re-exports: the derives `Params`, `ParamEnum`, `State`; the param
types `FloatParam`, `IntParam`, `BoolParam`, `EnumParam`, `MeterSlot`; helpers
`db_to_linear`, `linear_to_db`, `midi_note_to_freq`, `meter_display`;
`AudioTap`, `Arc`, `TAU`; and the plugin/process types used below.

Framework-independent DSP should live in its own crate that does **not** depend
on `truce`, so it can be unit-tested and A/B-compared without a host. The
`truce` wrapper crate stays thin.

## Minimal plugin skeleton

```rust
use truce::prelude::*;

#[derive(Params)]
pub struct MyParams {
    #[param(id = 0, name = "Drive", range = "linear(0, 24)", default = 0,
            unit = "dB", smooth = "exp(5)")]
    pub drive: FloatParam,
}

#[derive(Default)]
pub struct MyState {
    // per-instance DSP state; heap buffers sized in `reset`
    scratch: Vec<f32>,
}

pub struct MyPlugin;

impl PluginLogic for MyPlugin {
    type Params = MyParams;
    type DspState = MyState;

    // false = wipe DSP state on reset; true = keep it across reactivation
    const PRESERVE_DSP_STATE: bool = false;

    fn reset(state: &mut Self::DspState, params: &Self::Params, config: &AudioConfig) {
        // Allocate here (NOT in process). config.sample_rate, config.max_block_size.
        state.scratch.resize(config.max_block_size, 0.0);
        let _ = params;
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        for i in 0..buffer.num_samples() {
            let drive = db_to_linear(params.drive.read()); // read() once per frame
            for ch in 0..buffer.channels() {
                let (input, output) = buffer.io(ch);
                output[i] = input[i] * drive;
            }
        }
        ProcessStatus::Normal
    }

    fn editor(params: Arc<Self::Params>) -> Box<dyn Editor> {
        editor::create(params) // see egui section; or omit the method for no GUI
    }
}

truce::plugin! {
    logic: MyPlugin,
    params: MyParams,
}

truce::enable_rt_paranoid!(); // no-op unless the `rt-paranoid` feature is on
```

## Parameters (`#[derive(Params)]`)

Each field is one param. `id` must be stable forever (host automation/preset
key). `#[skip]` marks a non-param field (e.g. an `Arc<AudioTap<f32>>`); the
derive also accepts `#[nested]`/`#[meter]`/`#[persist]`.

```rust
#[derive(Params)]
pub struct P {
    #[param(id = 0, name = "Drive", range = "linear(0, 24)", default = 0,
            unit = "dB", smooth = "exp(5)")]
    pub drive: FloatParam,

    // Enum knob — no range needed; the derive fills Enum { count } from the type.
    #[param(id = 1, name = "Mode", default = 0)]
    pub mode: EnumParam<Mode>,

    #[skip]
    scope_tap: Arc<AudioTap<f32>>,
}
```

### Range strings (`range = "..."`)

| String                     | `ParamRange`             | Notes |
|----------------------------|--------------------------|-------|
| `linear(min, max)`         | `Linear`                 | plain linear map |
| `log(min, max)`            | `Logarithmic`            | strictly positive bounds; good for Hz |
| `skewed(min, max, factor)` | `Skewed`                 | `factor<1` gives the low end more knob travel |
| `sym_skewed(min,max,f,c)`  | `SymmetricalSkewed`      | center-detented (pan, EQ gain) |
| *(omit for `EnumParam`)*   | `Enum { count }`         | auto from `variant_count()` |
| *(IntParam)*               | `Discrete { min, max }`  | integer steps |

### Other `#[param]` keys

- `smooth = "exp(ms)"` — one-pole (multiplicative) smoothing.
- `smooth = "log(ms)"` — logarithmic smoothing. Omit for no smoothing.
- `unit = "dB" | "Hz" | "%" | ...` — display unit (`ParamUnit`).
- `default = <plain value>` — for `EnumParam`, a **0-indexed variant integer**
  (non-integer/out-of-range panics at construction).

### Reading params in `process`

- `FloatParam::read() -> f32` — returns the smoothed value **and advances the
  smoother one tick**. Call exactly **once per sample frame** (not once per
  channel), or smoothing runs at the wrong rate.
- `IntParam::value() -> i64`, `BoolParam::value() -> bool`.
- `EnumParam::<E>::value() -> E` (atomic load, RT-safe), also `.index() -> u32`,
  `.set_value(E)`, `.set_index(u32)` (clamps to the last variant).

### Enum params (`#[derive(ParamEnum)]`)

Derive on a C-like (unit-variant) enum. It generates `Clone, Copy, PartialEq,
Eq` plus the 5 `ParamEnum` methods. Display name defaults to the variant ident;
override with `#[name = "..."]`. Keep the framework enum in the wrapper crate
and map it to a plain DSP-crate enum so the DSP crate stays truce-free.

```rust
#[derive(ParamEnum)]
pub enum Mode {
    Clean,
    Detail,
    #[name = "Fold Back"]
    Fold,
}
```

The `Params` derive also generates a param-id enum named
`<StructName>ParamId` (e.g. `MyParamsParamId`), commonly imported as `P` and
used by the editor and `get_param_*` calls.

## Audio processing contract

- `buffer.num_samples()` / `buffer.channels()`.
- `buffer.io(ch) -> (&[f32] input, &mut [f32] output)` — per-channel slices.
- Return `ProcessStatus::Normal`.
- `AudioConfig { sample_rate: f64, max_block_size: usize }` arrives in `reset`.
- **Real-time rule:** no allocation, locks, or I/O in `process`. Size all
  buffers in `reset`. `PRESERVE_DSP_STATE` controls whether state survives a
  reset.

### Wait-free audio→GUI streaming: `AudioTap<T>`

Lock-free SPSC ring for pushing samples to the editor. Hold it as an `Arc` in
the params struct (`#[skip]`) so both the audio thread and editor reach it.

```rust
// audio thread (process): push interleaved frames from a preallocated buffer
params.scope_tap.push_frames(&state.transfer[..n * 2]);

// gui thread (editor): drain without allocating
params.scope_tap.drain_with(|samples: &[f32]| history.ingest(samples));

// on reset / editor open: params.scope_tap.clear();
```

## egui editor (`truce-egui`)

```rust
use truce_egui::{EditorUi, EguiEditor};
use truce_egui::widgets::param_knob;
use truce_egui::theme::{self, BACKGROUND, HEADER_BG, METER_CLIP, TEXT, TEXT_DIM};
use crate::{MyParams, MyParamsParamId as P};

struct MyEditor { /* GUI-only state, e.g. scope history */ }

impl EditorUi<MyParams> for MyEditor {
    fn opened(&mut self, state: &PluginContext<MyParams>) {
        state.scope_tap.clear(); // PluginContext derefs to the params
    }

    fn ui(&mut self, ui: &mut egui::Ui, state: &PluginContext<MyParams>) {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(16));
        ui.painter().rect_filled(ui.max_rect(), 0.0, BACKGROUND);

        // read a param for drawing (plain, un-smoothed):
        let ceiling = db_to_linear(state.get_param_plain(P::Ceiling));

        // bind a knob widget to a param id:
        param_knob(ui, state, P::Drive, "Drive");
    }
}

pub(crate) fn create(params: Arc<MyParams>) -> Box<dyn Editor> {
    EguiEditor::with_ui(params, (520, 320), MyEditor { /* .. */ })
        .with_visuals(theme::dark())
        .resizable(false)
        .into_editor()
}
```

Notes:
- `PluginContext<P>` is passed to editor callbacks and `Deref`s to the params
  struct, so `#[skip]` fields like `scope_tap` are reachable directly.
- `state.get_param_plain(P::X) -> f64` reads a param's current plain value for
  drawing.
- `param_knob(ui, state, P::X, "Label")` is the stock knob widget for a float
  param. The built-in `GridLayout` renders only param widgets/meters at a small
  fixed size (~140×120); for custom drawing (scopes, meters) implement
  `EditorUi` and paint with `egui::Painter` as above.
- `egui` is used directly (`use egui::...`) alongside the truce-egui helpers.

## Testing & RT verification

Drive `process` directly in unit tests with checked buffers:

```rust
let params = MyParams::default();
let mut state = MyState::default();
MyPlugin::reset(&mut state, &params, &AudioConfig::new(48_000.0, 4));

let inputs: [&[f32]; 1] = [&[0.25, 1.25, -1.5, 0.5]];
let mut out = [0.0; 4];
let mut outputs: [&mut [f32]; 1] = [&mut out];
let mut buffer = AudioBuffer::from_slices_checked(&inputs, &mut outputs, 4);

let events = EventList::default();
let mut out_events = EventList::default();
let transport = TransportInfo::default();
let mut ctx = ProcessContext::new(&transport, 48_000.0, 4, &mut out_events);

assert_eq!(
    MyPlugin::process(&mut state, &params, &mut buffer, &events, &mut ctx),
    ProcessStatus::Normal,
);
```

Allocation audit (needs the `rt-paranoid` feature):

```rust
let (_, allocations) = truce::rt::audit(|| {
    let _section = truce::rt::RtSection::enter();
    let _ = MyPlugin::process(&mut state, &params, &mut buffer, &events, &mut ctx);
});
assert_eq!(allocations, 0);
```

## Gotchas

- `id` values are a permanent contract — never renumber an existing param.
- Call each smoothed `FloatParam::read()` once per frame, before the channel
  loop, and reuse the value for every channel.
- Keep `EnumParam` defaults as integer variant indices.
- Do the drive/ceiling `db_to_linear` conversion in the wrapper; keep the DSP
  crate in linear amplitude and framework-free.
- Build/verify both bundles: `--features clap` and `--features vst3`.

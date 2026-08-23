# Roadmap

Committed future direction, in rough priority order. Promote items here from
`ideas.md` when decided; move items into `ctx.md` when they become the active
task.

## Next

Igorek build-out is the active task — see [`ctx.md`](ctx.md). Design:
[`igorek.md`](igorek.md).

## Later

- Add further `oiwn` plugins as sibling workspace crates, extracting shared DSP
  infrastructure only when a second concrete consumer requires it.
- Igorek: non-uniform (Gardner) partitioning to cut CPU on long room IRs — the
  8 s / 192 kHz corner is the documented worst case.
- Igorek: true stereo IR matrix (2-in × 4-conv × 2-out) instead of the diagonal
  L→L / R→R convolution.
- Igorek: drag'n'drop WAV loading if baseview-truce ever grows file-drop
  events (blocked today — `WindowEvent` has no file variants).

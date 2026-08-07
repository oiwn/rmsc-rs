# Ideas

Uncommitted possibilities — noted but not decided to do right now. Promote to
`roadmap.md` only when we decide to pursue something.

## Grey out Bogdan controls the current Mode ignores

Shape (Sine / Triangle) only does anything in Fold mode, but the dropdown looks
live in Clean and Detail — it invites a change that produces no sound. Same for
the other two dependent controls:

| Mode | Drive | Ceiling | Detail | Amount | Shape |
|---|---|---|---|---|---|
| Clean | yes | yes | — | — | — |
| Detail | yes | yes | yes | yes | — |
| Fold | yes | yes | — | yes | yes |

(In Fold the delta high-pass still runs every sample so its memory stays warm,
but its output never reaches the output — so the Detail knob is inert there.)

Truce has no per-parameter "inactive" flag: `ParamFlags` only covers
`AUTOMATABLE` / `MODULATABLE` / `MODULATABLE_PER_NOTE`, and the `truce-egui`
widgets take no enabled argument. So this is editor-side only — wrap the
affected `param_knob` / `param_dropdown` calls in `ui.add_enabled_ui(false, …)`,
or dim them by hand.

Open questions: does the parameter stay automatable while greyed (it should —
hiding it from the host would break existing automation), and is greying enough
or should the row reflow? Kirya has the same situation with Mod Rate / Depth /
Shape at Mod Depth 0, so whatever pattern lands here should suit both.

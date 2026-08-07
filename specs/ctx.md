# Current task context

**No active task.**

Kirya (Dattorro plate reverb) shipped on 2026-08-07 along with its follow-ups.
Both are recorded in [`../CHANGELOG.md`](../CHANGELOG.md); the plugin's own
description, signal chain, parameter reference and gotchas live in
[`kirya.md`](kirya.md).

## What goes here

The task currently being worked on, and only that. One task at a time.

```
ideas.md  ->  roadmap.md  ->  ctx.md  ->  CHANGELOG.md
 unsorted     committed      active      finished
```

Promote an item from [`roadmap.md`](roadmap.md) when it becomes active, write
the plan out here, then move the finished summary into `CHANGELOG.md` and clear
this file back to this state.

A task block usually carries:

- What it is and why, plus reference links.
- Decisions locked before starting, and refinements settled while building
  (record the ones that contradict the original plan — those are the ones worth
  keeping).
- A numbered, checkboxed step list.
- Verification: the commands to run, the tests to write named after observable
  behaviour, and the host checks that need ears.

Durable knowledge does **not** belong here. Anything still true after the task
ends goes to the plugin's `specs/<plugin>.md` (how it works, parameters,
gotchas, links) or to [`overview.md`](overview.md) (workspace architecture and
house rules). This file is scratch; those are the record.

## Next up

See [`roadmap.md`](roadmap.md).

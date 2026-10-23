# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring the codebase.

## Before exploring, read these

- **`CONTEXT-MAP.md`** at the repo root — it points to bounded contexts across the workspace. Read each context file relevant to the task:
  - [`crates/mkv-core/CONTEXT.md`](../../crates/mkv-core/CONTEXT.md) for media probing, scoring, asset resolution, font extraction, and batch remuxing.
  - [`desktop-tauri/CONTEXT.md`](../../desktop-tauri/CONTEXT.md) for desktop GUI, queue state projection, and the in-process Engine Bridge. Its terms also apply to `desktop-slint`, the Slint host being ported beside the Tauri one ([CONTEXT-MAP.md](../../CONTEXT-MAP.md), ADR-0017); the glossary moves with the host at the switch.
- **`docs/adr/`** — read ADRs that touch the area you're about to work in (`0001` through `0017`, noting the 2026-09-24 ratified amendments to ADR-0004 for subtitle/cover precedence and to ADR-0008 for font retention removal and typed track assignments).

If any of these files don't exist, **proceed silently**. Don't flag their absence; don't suggest creating them upfront. They are created lazily, when terms or decisions actually get resolved.

## File structure

This repository follows a multi-context layout governed by `CONTEXT-MAP.md`:

```
/
├── CONTEXT-MAP.md                     ← Bounded context map and relationships
├── docs/adr/                          ← System-wide and bridge architecture decisions (0001 - 0017)
├── crates/
│   └── mkv-core/
│       └── CONTEXT.md                 ← Media Engine Context
└── desktop-tauri/
    └── CONTEXT.md                     ← Desktop Application Context
```

## Use the glossary's vocabulary

When your output names a domain concept (in an issue title, a refactor proposal, a hypothesis, a test name), use the term as defined in the relevant `CONTEXT.md`. Don't drift to synonyms the glossary explicitly avoids.

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing language the project doesn't use (reconsider) or there's a real gap (note it for `/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently overriding:

> _Contradicts ADR-0008 (typed engine bridge and track assignments) — but worth reopening because…_

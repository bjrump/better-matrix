---
name: rune
description: Build and debug UI with Rune, the GPU-rendered Rust UI framework used by apps/desktop. Use when writing or changing views, components, layout, text input, scrolling, motion, windows, or when rendering the app headless to check UI work.
---

# Rune

Rune (github.com/anomalyco/rune) is pre-release and changes often. Never rely on memory for its
API: read the docs and examples synced at the exact rev this repo pins.

## Get the reference material

`.agent-tools/rune/` is gitignored. If it is missing, or `.agent-tools/rune/REV` differs from the
`rune` rev in the root `Cargo.toml`, run `bun run agent-tools`.

- `.agent-tools/rune/docs/rune.md`: the core API (elements, layout, text fields, scrolling, app lifetime, `AppHandle`, headless tooling). Large: search it by heading.
- `.agent-tools/rune/docs/rune-kit.md` and the other crate READMEs.
- `.agent-tools/rune/docs/AGENTS.md`: Rune's own rules (state machines, performance).
- `.agent-tools/rune/examples/`: working apps. `todo`, `settings`, `web-hello` (chat with composer) and `launcher` are good starting points.
- The compiled source is in Cargo's git checkout (`~/.cargo/git/checkouts/rune-*/<short rev>/crates/rune/src`) when a signature is in doubt.

## Check your work headlessly

```sh
cargo run -p better-matrix-desktop -- --snap /tmp/shot.png --scene demo   # render a PNG
cargo run -p better-matrix-desktop -- --tree --scene demo                 # dump the element tree
```

Look at the PNG after every visual change. For behaviour, write a test with `rune::Runtime`:
build the state, `rt.frame(0.0)`, `rt.find("key")`, `rt.pointer_down/up`, then assert on `rt.state`.

## Rules that bite

- Give interactive and list elements stable `.key(..)`s; tests and scroll state depend on them.
- Views are pure functions of state. Work off the UI thread goes through `AppHandle::update` or `rune::Latest`; never block a handler.
- `TextEdit` holds field state; attach with `.on_edit(|s| &mut s.field)`.
- Modes are explicit state machines (`rune::machine`), not clusters of booleans.
- No file I/O during frames.

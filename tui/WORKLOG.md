# TUI work log — patch 0001

**What this is:** the first real TUI change. Not cosmetics — this removes four ways the
fork could damage or mislead its own users.

Apply it:

```bash
cd zenithal
git apply /path/to/zenithal/patches/0001-zenithal-tui-brand.patch
cargo check -p codex-tui -p codex-cli     # needs a machine that can build ~2.1M LOC
cargo test  -p codex-tui --test brand_urls
```

**Verification done here** (sandbox has 2 cores / 1 GB, so no full build):

| Check | Result |
|---|---|
| `git apply` on a clean clone of `main` | **applies cleanly** |
| `brand_urls.rs` guard lands and runs | yes (6.6 KB, std-only, no new deps) |
| Guard vs **pristine upstream** | **24 hard hits → fails**, i.e. it really catches the bugs |
| Guard vs **patched tree** | **0 hard hits → passes**, 21 soft (audited, non-failing) |
| Compilation | **not verified here — you must run `cargo check`** |

## The four real problems this fixes

### 1. Self-update would replace your fork with OpenAI's CLI — *the worst one*

`codex-rs/tui/src/update_action.rs` built these commands to "update" the CLI:

```
npm  install -g @openai/codex
bun  install -g @openai/codex
pnpm add     -g @openai/codex
brew upgrade --cask codex
sh -c 'curl -fsSL https://chatgpt.com/codex/install.sh | sh'
powershell -c '$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex'
```

Press "update" in the TUI and you get OpenAI's `codex` binary installed over your work.
Fixed by making `from_install_context` return `None` and neutralizing every branch, so no
code path can emit an OpenAI-installing command even if the guard is later weakened.

Also fixed: `tui/src/updates.rs` (`LATEST_RELEASE_URL`) and `cli/src/doctor/updates.rs`
(`GITHUB_LATEST_RELEASE_URL`) checked **upstream's** releases to decide whether you were
out of date. That is now our repository, so "Update available" means *our* release.

### 2. The TUI downloaded OpenAI's announcements

`tooltips.rs` fetched `raw.githubusercontent.com/openai/codex/main/announcement_tip.toml`
and rendered it in the UI. Your fork would display OpenAI's messaging as its own.
Repointed to our repo; `announcement_tip.toml` is included in the patch with a first
(first-party) announcement. An empty `ANNOUNCEMENT_TIP_URL` now disables the feature.

### 3. `zenithal app` installed OpenAI's signed desktop binary

`cli/src/app_cmd.rs` → `desktop_app/mac.rs` downloaded
`persistent.oaistatic.com/codex-app-prod/Codex.dmg` (and the Windows MSIX `OpenAI.Codex`)
and installed it. Disabled with a clear error pointing at our tracker; the dead module is
kept compiling but marked `zenithal:allow-upstream-url` and is unreachable.

### 4. Bug reports and update notices sent users upstream

- `feedback_view.rs` → `github.com/openai/codex/issues/new` (you can't triage those)
- `history_cell/notices.rs` → "See github.com/openai/codex for installation options" and
  upstream release notes
- `doctor` printed `npm install -g @openai/codex` as the remediation

All repointed at `Hubris-tic/zenithal`.

## Identity changes (20 strings)

Onboarding heading, session header, `Zenithal (vX.Y.Z)` version line, status-line and
terminal-title labels, composer placeholder, four `/help` descriptions, upgrade banner,
daemon hint.

## The guard: `codex-rs/tui/tests/brand_urls.rs`

The important part, because it makes the fix permanent. It scans shipped source (skipping
`*_tests.rs`, `tests/` dirs, and everything from the first `#[cfg(test)]` module) for two
tiers of pattern:

- **hard** — fails the build. Supply-chain and misrouting only: `@openai/codex`,
  upstream release/issue/API URLs, upstream announcement feed, `chatgpt.com/codex/install*`,
  `oaistatic.com/codex-app-prod`, `--cask codex`.
- **soft** — printed, never fails. ChatGPT-account features a BYOK user never reaches
  (billing banners, `help.openai.com` articles) plus deliberate security policy
  (`exec_server_command.rs` restricts credentials to `openai.com`/`openai.org` hosts).

A `zenithal:allow-upstream-url` comment (looked for up to 3 lines above) is an explicit,
auditable exception. Every current one is annotated with why it is inert.

Why two tiers: a guard that fails on cosmetic debt gets disabled within a month. A guard
that fails only on "this could install someone else's software" survives.

## Corrections to my earlier inventory

- I listed `auth.openai.com` in `custom_terminal.rs` as urgent. **Wrong** — it is inside a
  `#[cfg(test)]` module (hyperlink-wrapping fixture). It is not shipped. Category C.
- I said "185 user-visible strings". The real, more serious number: **24 supply-chain /
  misrouting references** in shipped code, of which this patch fixes all of them.
- The 21 remaining soft references are ChatGPT-account features. They are in
  `rebrand/strings.md` as debt, not as risk.

## Still to do (needs a build to verify)

| Task | Why it needs a real compiler |
|---|---|
| Rename `[[bin]] name = "codex"` → `zenithal` | tests may reference `CARGO_BIN_EXE_codex` |
| Config home `~/.codex` → `~/.zenithal` with fallback | touches `find_codex_home` call sites |
| Delete `desktop_app` + doctor signature checks outright | needs the module graph to stay consistent |
| Prompt scaffolding (`## My request for Codex:`) | changes model behaviour → cassette updates |
| Wire `zenithal-brand` constants in place of literals | mechanical, but `cargo check` should drive it |

## Next TUI work, in order

1. **Squad board** — the multi-agent view is the reason someone switches. Needs the
   orchestrator first (`AGENTIC-UPGRADE-PLAN.md` §3C).
2. **Memory graph view** — renders the hybrid store; depends on §3A.
3. **Diff review surface** — the most-used screen; can start now, no dependency.
4. Web UI after the TUI proves the protocol shape.

---

# Part 2 — views built (this session)

## What exists now

| Path | What it is | Verified |
|---|---|---|
| `zenithal/ui-core/` | pure view-model logic: diff engine, mission state machine, graph layout | **32 tests pass**, 0 warnings |
| `zenithal/tui/src/theme.rs` | the palette: red + black first, everything else punctuation | 4 tests |
| `zenithal/tui/src/chrome.rs` | the shared frame: mark, tabs, one rule, one footer | 9 tests |
| `zenithal/tui/views/` | ratatui views: `diff_review`, `mission_board`, `memory_graph` | **compiles against ratatui 0.29**, 37 tests pass |
| `zenithal/tui/preview/*.png` | **real renders** of the views (see "Preview, not mockup") | 1220×644 each |
| `zenithal/tui/examples/snapshot.rs` | draws the views into ratatui's `TestBackend` and dumps cells | `cargo run --example snapshot` |
| `zenithal/tui/render_preview.py` | rasterises that cell dump with a monospace font | runs in ~1s |

**Total in these crates: 40 tests, 0 warnings.** Whole project now: **96 tests**.

## Why the split matters

Every hard decision is in `ui-core` with no ratatui types, so it is unit-testable in CI
without a terminal:

- **diff**: hunk parsing, added/deleted/renamed detection, pairing removals with additions
  side-by-side, folding long context runs, and character-level inline diffing (`inline_diff`,
  LCS-based, unicode-safe).
- **mission**: legal state transitions (terminal states never move), stable display ordering
  with Strategos pinned above its operators
  (needs-approval first, ties broken by role then id so the board never reshuffles under
  the cursor), budget guard.
- **graph**: layered BFS layout with deterministic rank/column assignment, truncation by
  rank-then-score, and links that can only reference placed nodes.

The views then only position those models. Three tests on the render side: `fit()` never
exceeds its width, agent rows never exceed the given width, and layout memoization
invalidates correctly.

## Two bugs my own tests caught while writing this

1. **`s.name` on `Summary`** — the squad board title read a field that does not exist;
   caught immediately by the type checker.
2. **Agent rows overflowed below ~70 columns** — the fixed column plan summed to 71 chars,
   so a narrow terminal would wrap rows. `agent_row` is now responsive: it drops branch,
   then the progress bar, then cost, then state, always keeping glyph/role/task. That is
   the difference between "works on my 160-col terminal" and shipping.

## Preview, not mockup (this replaced the mockup)

The first version of this work shipped `mockup-mission.png`, a hand-laid-out picture drawn by
`render_mockup.py`. It looked good and it was a liability: it was drawn from *rules* the code
was supposed to follow, so the two could silently drift, and publishing it as a screenshot
would have been a lie.

That file is gone. In its place:

```bash
cargo run --example snapshot      # real views -> ratatui TestBackend -> preview/*.json
python3 render_preview.py         # that cell grid -> preview/*.png
```

`examples/snapshot.rs` calls the **shipped** `render_screen` functions of all three views; the
JSON is a cell-by-cell dump (symbol, foreground, background, weight); the Python file is a
dumb monospace rasteriser that knows nothing about missions or diffs. If a preview looks
wrong, the view is wrong. There is no second source of truth left to drift.

It also found two real bugs immediately: the memory view rendered **zero nodes** because its
focus was being set to a node's label instead of its id, and a stray bold space in the diff
half-separator coloured pixels nobody asked for.

## Wiring into the fork (next commit, needs a real build)

`zenithal/tui/views/` becomes `codex-rs/tui/src/views/` and `zenithal-ui-core` becomes a
workspace member:

```rust
// codex-rs/tui/src/lib.rs
mod views;                       // new module, no upstream file restructured
pub use views::{diff_review, memory_graph, mission_board};
```

Add to `codex-rs/tui/Cargo.toml`: `zenithal-ui-core = { path = "../zenithal-ui-core" }`, and
match the workspace's pinned `ratatui` version (this crate used 0.29; if the workspace is
newer, adjust `Borders`/`Block` APIs accordingly).

The palette is deliberately local (`views/mod.rs::palette`) so these views do not depend on
upstream's theme API. Wiring them to `crate::theme` is a follow-up that also gets you light
themes for free.

## What is still missing (honest list)

| Gap | Why it is not done |
|---|---|
| No key handling | Views expose `down`/`up`/`next_file`/`prev_file`/`deeper`/`shallower`; binding them needs the fork's keymap (`keymap.rs`) which I cannot compile-test here |
| No real data source | Squad board needs the orchestrator; memory graph needs the SQLite store. Both are separate milestones |
| No scrollbar rendering | `scroll` exists in state; drawing an indicator is cosmetic and unverified |
| Diff view is read-only | Accept/reject per hunk needs the patch-application path in `apply-patch` |

## Suggested order from here

1. Wire the views in behind a `--view squad|diff|memory` flag, real screenshot, commit.
2. Memory store (SQLite + vec + graph) → makes `memory_graph` live.
3. Squad orchestrator → makes `squad_board` live.
4. Then the web UI, which reuses the same view models over the existing app-server JSON-RPC.

## Addendum — the multi-agent model changed after this patch

The first draft of this view assumed a peer squad with an integrator. That design was
replaced: the orchestrator is now one **Strategos** (boss, no tools) and N **operators**
(one git worktree each), and there is no integrator and no merge step — accepted files are
copied into the working tree whole, first accepted writer wins. The view model followed:
`ui-core/src/squad.rs` became `ui-core/src/mission.rs` and `views/squad_board.rs` became
`views/mission_board.rs`, with the boss pinned to the top row and a rule separating it from
the agents it commands. Counts above are the current ones (ui-core 32, views 18).

## Addendum — the black hole (patch 0002)

The product is `Zenithal`, and the identity is a black hole: a void that absorbs light, an
accretion disk at the hot end of red, and the cold blue of the light that escaped. This
addendum is what that cost and what it bought.

### The palette moved, and stayed red

`src/theme.rs` was rewritten as the physics of one object: `VOID #05060a` (page and horizon,
cold by test: blue ≥ red, luma < 16), `RAISED`, `LINE`, `LINE_RED`, `RED #e5484d`,
`RED_HOT`, `RED_DEEP`, `EMBER #ff9a4d`, `PHOTON #9cc7ff`, `PHOTON_DIM`, `JET`, `BONE`, `GREY`,
`FAINT`. Two things are new in kind rather than in value:

* **`heat(h)`** — the disk ramp as a function, deep red → red → ember → bone, clamped, tested
  for monotonic brightness and for clamping. The animation asks for a temperature and gets a
  colour; it never picks one.
* **`badge()`** — the one inverted element, now void-on-red rather than black-on-red, so the
  alert belongs to the same surface as everything else.

Accepted is no longer green. It is `PHOTON`: the palette has exactly one cool accent, and
"this work made it out" is what it means.

### Choosing an OSS library, and rejecting two

The brief was "add a black hole animation from OSS libraries, don't hand-wave the physics".

| Candidate | Verdict |
|---|---|
| `austindelic-blackhole-ratatui` | **rejected** — drags in `wgpu` 29 and ~139 packages and needs a GPU adapter. Violates local-only, no-heavy-deps |
| `black_hole`, `blackhole_renderer` | **rejected** — same wgpu requirement |
| `sagittariusas` 0.0.3 | **rejected after trying** — the physics is right (Kerr shadow, ISCO, photon ring, Doppler asymmetry) but it pulls `sciforge` 0.0.2: 627 source files, 92 kLOC, and `rustc` was SIGKILLed compiling it in this 2 GB sandbox. A dependency that cannot be built is not a dependency |
| **`nullgeo` 0.2** | **chosen** — a real general-relativistic ray tracer (Schwarzschild, Kerr, Kerr–Schild, Reissner–Nordström, Ellis), MIT, MIT-clean dependency set (`nalgebra` and friends, ~10 crates), CPU only, and it renders exactly what this screen needs: `trace_geometry` returns per-pixel ray class, disk crossing radius, and the gravitational `g` factor |

`views/blackhole.rs` is therefore not an animation of a shape: it is a terminal-sized render
of an integrated image.

### The five decisions inside the view

1. **One ray per cell, one colour per cell.** Terminals have no subpixel colour; drawing a
   colour per braille dot means a thousand spans and confetti. A cell takes the colour of its
   hottest dot.
2. **Trace once, animate the flow.** The raster is cached per terminal size; `tick(dt)` only
   moves the Keplerian swirl. First frame ~0.2 s at 80×24 in release, every frame after that
   is arithmetic.
3. **Two criteria for the photon ring.** "Skimmed the photon sphere" is the physical
   definition and finds the lensed arcs; "touches the shadow's boundary" finds the one-cell
   critical curve that a coarse raster would otherwise lose under the disk's own emission.
   Either alone leaves the ring with holes.
4. **Flux, then contrast.** `shakura_sunyaev_temperature` to the fourth power is flux, which
   varies about fourfold across this disk — enough for a camera, not enough for 132 columns.
   Squaring it again keeps the ordering and gives the falloff an eye expects.
5. **Fall back in process.** If the tracer errors, an analytic Schwarzschild raster is used
   and the facts line says `analytic · fallback`. A screen that cannot say which engine drew
   it is a screen you cannot trust.

### What is on the screen now

The header carries a fourth tab, `hole`, and the mark became the sigil (`◍`): the product
name is `Zenithal`, one word, no "code" in it anywhere. The frame's tab cells gained their
separator so an active tab can never fuse to the one before it (`memory▸ hole` was a bug, not
a style), and a narrow terminal now drops tabs from the end before it drops the mark.

Under the image, three numbers and a phase: shadow coverage, the disk's traced radial range,
the ISCO, and the engine. `preview/hole.png` and `preview/hole-narrow.png` are the shipped
render; `preview/hole.gif` is 48 clock values of it; `cargo run --release --example hole` is
the same thing live in a terminal.

### Counts

`zenithal-tui-views` went from 37 tests to **53**: palette invariants, the heat ramp, tab
geometry with four tabs, and the black hole's own properties — the shadow is a hole in the
middle of the frame, the disk is traced inside the physics' own radii, a photon ring wraps the
shadow, the heat falls off outward from the Shakura–Sunyaev peak, the star field is sparse and
non-repeating, the clock only moves when ticked and wraps, the picture seethes between two
clock values, the ring does not hold one colour, the jets flicker on the axis, and the fallback
draws rather than blanks.

The palette is no longer local to the views: `views/mod.rs::palette` is gone, and every view
goes through `crate::theme`. That removes the follow-up this document used to list.

## The animation, second pass

The first version moved, but it moved like a lamp behind a curtain: one sine on the brightness,
so the disk *pulsed* instead of shearing. Four changes, all in `plot()` and `density()`:

1. **Three flow modes, not one.** A two-armed spiral advected at the local Keplerian rate
   (`omega = r^-1.5`), a three-armed harmonic running the other way so the pattern interferes
   with itself rather than sliding, and a fine filament wound by `ln r` so the inner disk
   visibly shears ahead of the outer disk. Plus a slow radial flicker, because a flow that is
   perfectly periodic reads as a texture that is merely translating.
2. **A halftone instead of four patterns.** Brightness is now a 2×4 ordered dither: eight dot
   positions per cell lit in a fixed `BAYER` rank order, so heat maps to dot *count*. The old
   four-step version drew visible horizontal bands. A per-cell hash perturbs the level by ±half
   a dot, which turns the faint outer disk from a grid of identical specks into speckle.
3. **A tone curve.** `raw / (1 + raw)` (Reinhard) after the display gamma. Without it every
   cell above the knee clipped to a solid block and the disk became one flat wall of dots; with
   it the inner edge is white-hot and the outer disk is dim, which is the gradient the physics
   actually has.
4. **A ring that breathes and jets that flicker.** The photon ring's colour beats between
   `PHOTON_DIM`, `PHOTON` and `BONE` per octant, so the critical curve shimmers like a caustic
   rather than sitting there as a painted circle. The jets got a travelling knot plus a fast
   flicker; a perfectly steady jet is a line drawn over the picture.

The disk's outer radius came in from 11 to 8.5 r_s and the display gamma is 0.85: at 132
columns the wider disk was mostly empty area, and the narrower one frames the shadow the way
the EHT image does.

Three new tests hold this down: the picture must actually change between two clock values
(more than 10% of lit cells, fewer than 95% — the second bound rejects noise), the photon ring
must not hold one colour over a full turn, and the jets must flicker *and* stay within two
cells of the spin axis. `zenithal-tui-views` is now at **53**.

## The animation, third pass: things that move, not things that pulse

The disk was alive; everything else on the screen was a still photograph. A TUI that shows a
running mission with a frozen board is lying about the one thing it knows for certain — that
work is happening. Three additions, and one of them is a subtraction.

**1. A running operator spins.** `MissionBoardState` gained a clock (`tick(dt)`, clamped to
250 ms so a laptop that was closed for an hour does not teleport the animation) and the state
column now draws a braille spinner at ten frames a second while an operator runs. The braille
family is deliberate: the black hole screen is drawn in braille dots, and a block spinner next
to it would look like a second renderer had been bolted on. `agent_row` is unchanged for
callers that do not have a clock; `agent_row_at` takes one.

**2. The newest feed line flashes, then fades.** When the activity feed grows, the new line is
accent-red for 800 ms and then settles. This is the entire animation budget of the feed: one
line, briefly brighter, nothing moving that the eye is trying to read. `flash()` is a pure
function of the clock and the length of the feed, so it is tested without waiting.

**3. The black hole got a hot spot.** The turbulence was *statistical*: brightness changed
everywhere at once, so the disk seethed but nothing orbited. There is now a single Gaussian
knot at 4.1 M, carried round at 1.15 rad/s — slower than the local orbital frequency, because
the knot is a pattern in the flow rather than a particle, and a spot that laps the disk every
frame reads as a strobe. It is the one feature with an identity across frames, and it is what
makes the disk read as orbiting. Stars twinkle on their own phases (small amplitude: a star
that blinks out is a bug), and the photon ring's beat gained a second harmonic so the crest
does not repeat on an obvious period.

**4. The subtraction: the disk no longer stops dead.** The tracer's annulus ends at `R_OUT`
and the screen was drawing that edge as a stamped circle. The outer quarter of the disk now
dissolves (`outer_fade`), and the inner edge gets a rim boost (`inner_rim`) so there is a
feature to follow round the shadow. The top of the brightness range is also pushed into solid
cells — a terminal has no antialiasing beyond dots, so the brightest plasma is now a body and
the only texture on screen is the cooling edge.

Measured cost of all of it: **zero**. The geometry is still traced once per terminal size and
cached; a frame is a few thousand sines. The `hot_spot` and `outer_fade` maths are `exp` on
numbers already in a register.

`zenithal-tui-views` is now at **60** tests: the clock clamps, a spinner advances and a settled
state never spins, the feed flashes and fades, the hot spot is bright on its own azimuth and
dark half a turn away, the rim outshines the disk behind it, and the outer edge reaches exactly
zero.

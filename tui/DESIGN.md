# The Stoic Codex interface: a black hole, drawn in red

A terminal UI lives or dies on two things: whether you can find the one thing that matters in
a second, and whether the screen is calm enough to read for an hour. This is the ruleset that
gets both, and every rule below is enforced by a test.

The identity is one object — a black hole — so the palette is the physics of one: a void that
absorbs light, an accretion disk at the hot end of red, and the cold blue of the light that
escaped. Red and black are the primaries, as they have been throughout this project. The black
hole did not change that; it gave the red a reason.

## The palette

Defined in `tui/src/theme.rs`; nothing else in the crate names a colour.

| Role | Colour | Where it appears |
|---|---|---|
| Void | `#05060a` | the page, and the inside of the event horizon. Cold, not grey |
| Raised | `#12141d` | one selected row; almost invisible on purpose |
| Line / line red | `#232633` / `#461c22` | hairlines, and the rule under the boss |
| Primary | `#e5484d` | the sigil, the active tab, the cursor, "removed", failure |
| Red hot | `#ff645c` | failure text, the failing member |
| Red deep | `#351318` | tint behind removed lines |
| Ember | `#ff9a4d` | the hot end of the disk; a conflict; "needs a human" |
| Photon | `#9cc7ff` | the photon ring and accepted work — the one cool accent |
| Jet | `#6f8bd8` | the relativistic jets, only in the animation |
| Bone | `#e8e4e0` | names and values — about 90% of the glyphs on screen |
| Grey | `#9997a3` | assignments, metadata |
| Faint | `#5e5c68` | labels, gutters, hints, line numbers |

Explicit RGB, never the terminal's `Red`/`Green` slots: a 16-colour theme would render this
design as something else entirely on someone else's machine. Tests assert that every constant
is an `Rgb` triple, that the page is near-black and cold, that the primary is still red, that
body text clears 5.5:1 against the void, and that the photon blue is genuinely cold against a
red disk.

`theme::heat(h)` is the palette as a function: the animation asks "how hot is this part of the
disk" and gets a colour back — deep red → red → ember → bone, clamped. No view invents a
colour, and the ramp has its own test.

## Six rules, each with the test that holds it

1. **Red is identity and focus, and one region gets one red thing.** The active tab, or the
   selected row, or the failing state — not all three at once. `/tui` tests assert the exact
   set of red spans in the header and in a board row: if a new red glyph appears, a test
   fails and the decision has to be made deliberately.
2. **No boxes.** Separation is a hairline (`─`) or a blank line. Borders cost four cells and
   say nothing a rule cannot. All four screens draw borderless.
3. **One inverted element at most.** Only a footer alert is a solid red block with black
   text. Everything else is text on the page.
4. **Column layout is computed once and shared.** The mission board's header and its rows are
   produced by the same `Columns::for_width` plan and written with `push_at`, so a header cell
   and a data cell cannot drift. The header's tabs and the red rule under them use one
   `tab_offsets` table for the same reason.
5. **Adapt by dropping, never by wrapping.** Below 68 columns the branch goes, below 46 the
   meter goes; rows never wrap, and tabs are dropped from the end before the mark is. The diff
   switches from side-by-side to a full-width unified stream below 72 columns in its pane. A
   test renders compact layouts at 24/40/60/80/100/160 columns and asserts no line exceeds its pane.
6. **Nothing is drawn that is not data.** No fake rows, no placeholder sparklines, no "N
   items" that does not add up. A view can only show what the view-model gave it. The black
   hole is the one sanctioned exception, and it is not an exception in spirit: every cell on
   that screen is an integrated null geodesic or a labelled backdrop.

## The frame

Every screen shares one frame, drawn by `src/chrome.rs`:

```
 ◍ Stoic Codex  │ ▸ mission   diff   memory   hole              on main   ● 1 running   $1.20
─────────────                          ────────────────────────────────────────────────
  ...body...
────────────────────────────────────────────────────────────────────────────────────────
 ● 3 operators  1 accepted                     ↑↓ select · enter detail · n/k rows · q quit
```

* The red rule under the header is exactly the width of the active tab, made from the same
  tab-offset table that placed the label.
* The right-hand status run is right-aligned as one unit and dropped from the right when it
  does not fit — never truncated into nonsense. At 40 columns you get the mark and some tabs;
  the mark is the last thing to go.
* The footer carries state on the left (chips) and the keys that work *on this screen* on the
  right. Hints are per-view; when the full string will not fit, a compact `? help` route wins
  over a status chip. The key guide itself is contextual to the active view.

## First contact: welcome and contextual keys

The welcome view reuses the shared header/footer without inventing an active tab. Its only brand
art is the black-hole sigil already used by the animation; the copy stays short, borderless, and
centered. Workspace, provider/model, and recent-mission details are rendered only when the caller
supplies them — no pretend connection state or placeholder project.

The `?` key guide is a raised-void overlay with no outline box. Each scope (welcome, mission, diff,
memory, hole) lists only that screen's controls, with keys in primary red and explanations in grey.
At 40 columns the sheet fills the available width, so no one-cell strip of underlying content leaks
around the modal; a regression test protects that edge case. The app shell still owns input
dispatch; the view and its snapshots are ready for the `?`/Escape handler when that shell is wired.

## What each screen spends its red on

| Screen | The red | Everything else |
|---|---|---|
| Mission board | the cursor bar, the running member's glyph + context meter, the rule under the boss | names in bone, branches and assignments in grey, accepted in photon blue |
| Diff review | removed lines (tinted `#351318`), the selected file's cursor | added lines in photon blue, context in grey, gutters faint |
| Memory graph | the focus node | link kinds and scores in grey; photon for symbols, ember for decisions, red only for an error node |
| Black hole | the sigil in the header, the hot inner edge of the disk | the disk ramps deep red → ember → bone by temperature; the photon ring is the only blue |

## The diff at forty columns

Side-by-side is the clearest comparison when both halves have room. Below 72 columns in the diff
pane it stops being a comparison and becomes truncation, so the view switches to a unified stream:
context appears once, paired removals/additions become two adjacent lines, line numbers stay where
there is room, and removed code keeps the same restrained red wash. The title labels the mode so
the change in reading direction is explicit. The 40-column snapshot is part of the example suite.

## The black hole screen

The fourth tab is not decoration: it is a reasonably honest picture of Sagittarius A*, drawn
on a terminal.

**Physics.** `tui/views/blackhole.rs` integrates null geodesics through the Kerr metric with
[`nullgeo`](https://crates.io/crates/nullgeo) 0.2 (MIT, CPU-only, ~10 transitive crates, no
GPU, no async, no network). One ray is traced per terminal cell from a camera at 88 `M` and
57° above the equatorial plane, through a disk whose inner edge is at the ISCO of an `a = 0.9`
hole and whose outer edge is at 8.5 `M`. What comes back is:

* **the shadow** — rays that never came out, drawn as the page itself;
* **the photon ring** — cells on the shadow's boundary and rays that skimmed the photon
  sphere, drawn cold, because they are the only light on the screen that has been somewhere
  the disk has not;
* **the disk** — the crossing radius turned into a Shakura–Sunyaev temperature, raised to the
  fourth power for flux, then a display gamma (`0.8`) and a Reinhard tone curve because the
  flux spans nearly two orders of magnitude and a terminal needs a falloff it can show. A rim
  boost just outside the ISCO gives the eye something to follow round the shadow, and the outer
  quarter of the disk dissolves instead of ending at a stamped circle;
* **beaming** — the left/right asymmetry is `g`, the observed-over-emitted energy ratio the
  tracer returns. The bright side of the disk is bright because of relativity, not because
  someone put a highlight there;
* **stars** — a hashed backdrop. The sky used for the trace is uniformly black, and a
  featureless sky has nothing to lens, so the stars are honest texture rather than a sky map
  being modelled badly.

**Cost.** The integration is the expensive part (about 0.2 s for an 80×24 frame in release),
and the camera does not move, so the raster is traced once per terminal size and cached
inside `BlackHoleAnim`. A tick after that is a few thousand sines: the animation is cheap
because the geometry is not recomputed. `tick(dt)` is the only clock, it takes wall-clock
seconds, clamps a suspended laptop to a quarter second, and wraps at 2π.

**Animation.** The visible motion is the accretion flow, in three modes so it seethes rather
than pulses: a two-armed spiral advected at the Keplerian rate (period ∝ r^1.5, so the inner
disk whips round while the outer disk crawls), a three-armed harmonic running the other way so
the patterns interfere instead of sliding, and a fine filament wound by `ln r` so the inner
disk shears ahead of the outer disk. A slow radial flicker keeps it from reading as a texture
that is merely translating. On top of the statistical motion there is one *feature*: a hot spot
— a Gaussian knot at 4.1 `M` carried round at 1.15 rad/s — which survives many frames, so the
eye can track it, and that is what makes the disk read as orbiting rather than flickering. The
photon ring beats between photon-dim, photon and bone per octant like a caustic, on two
harmonics so it does not repeat on an obvious period; two faint jets (`j`) travel and flicker;
the stars twinkle on their own phases, small enough that none of them blinks out. Brightness is
a 2×4 ordered dither — eight dot positions per cell lit in rank order, perturbed by a per-cell
hash — so heat maps to dot *count* and the faint outer disk is speckle rather than a grid, with
the brightest cells pushed into solid bodies.

**The rest of the app moves too.** A board that shows a running mission and does not move is
lying about the one thing it knows. `MissionBoardState::tick(dt)` advances a clock (clamped to
250 ms, so a suspended laptop cannot teleport the animation), a running operator's state glyph
becomes a braille spinner at 10 fps — the same dot family the hole screen draws with — and the
newest activity line is accent-red for 800 ms and then fades. That is the whole animation
budget of the feed: one line, briefly brighter, nothing moving that the eye is reading. The
snapshot previews are taken at clock 0, so they are reproducible.

**Degrading.** If the tracer ever returns an error, the raster falls back to an analytic
Schwarzschild shadow and the facts line says `analytic · fallback` instead of `kerr a=0.9 ·
nullgeo`. Nothing on this screen reaches the network or needs a GPU, so the fallback is
belt-and-braces rather than a real risk.

**Credit.** `nullgeo (MIT)` is printed in the footer of the screen itself, not just in this
file. A vendored physics engine that nobody can see is a physics engine nobody can check.

## Anatomy of the mission board

The board answers three questions in one glance, top to bottom:

```
 CODE       BRANCH             CONTEXT      RESULT     ASSIGNMENT          │  ✓  Strategos
 ◆ Strategos —                 ▰▰▰▰▰▰▰▰▰▰ done       replace sha256 …    │  ─────────────
 ────────────────────────────────────────────────────────────────────────  │  assignment  …
 ▶ x7-trace  stoic-codex/x7-trace  ▰▰▰▰▰▰▱▱▱▱ running    rotate the key …   │  branch      …
 ✓ x1-write  stoic-codex/x1-write  ▰▰▰▰▰▰▰▰▰▰ accepted   hash and verify …  │  verdict     …
 ACT                                                                      │  base        main
 00:02 ◈ x1-write  slot-1 → x1-write · the primary logic generator        │  plan        3 assign…
 00:03 → x1-write  deployed on stoic-codex/x1-write                          │  applied     1 accepted…
 00:26 ! x5-patch  src/error.rs also written by x1-write; x1-write wins  │  budget      $1.20 of $3.00
 01:04 ●           1 file applied, 1 conflict reported                    │
```

The left column is a **code**, not a name: an operator is `x1-write` or `x7-trace`, never
`agent 2`. Strategos names each one from the catalogue at plan time, and the feed records the
naming (`slot-1 → x1-write · the primary logic generator`) so a row that says `x7-trace` cannot
be mysterious.

1. **The table** — who exists, where they are, how much of their window they have spent, and
   what Strategos decided. Enough rows for the fleet, never more than half the pane.
2. **A hairline**, then **the feed** — what is happening this second, newest at the bottom,
   newest 16 lines or so. Only two kinds of line are coloured: a conflict (ember) and the end
   of the mission (red). Everything else is history in grey.
3. **The report pane** — the selected member's assignment, branch, verdict and reason, and
   under a rule, the mission's own numbers. This is why the right-hand side is never a blank
   rectangle.

## The context meter

The column that used to be a decorative "progress" bar is now **context window used** — an
agent cannot know how much of its task is done, but the loop knows how full the window is,
and that is the number that predicts a run about to forget its own earlier work. Red counts
down, so a nearly-full window reads as nearly empty and alarming rather than reassuringly
full.

## Previews

`preview/*.png` and `preview/hole.gif` are rendered from the real views, not drawn by hand:

```bash
cargo run --example snapshot      # shipped render_screen → TestBackend → preview/*.json
python3 render_preview.py         # cell grid → PNG
cargo run --example frames        # the hole, 48 clock values → preview/frames/*.json
python3 render_preview.py --gif   # → preview/hole.gif
cargo run --release --example hole  # and the same thing, live, in a terminal
```

The JSON is a per-cell dump (symbol, fg, bg, bold); the rasteriser is a dumb monospace drawer
that knows exactly one piece of typography trivia: a braille cell is eight dots, so it draws
eight dots. If a picture is wrong, the view is wrong — which is how the memory view's
zero-node bug was found the minute the first preview was generated.

**Verified 2026-10-26:** stable Rust 1.99.1 passes all **69 TUI tests**. The snapshot example
regenerated 12 cell-grid JSON files; the frame example regenerated 48 animation grids; the
renderer produced 12 PNGs and a 48-frame `hole.gif` (788×428, 120 ms per frame). GIF metadata
and sampled-frame differences were checked, and all static wide, narrow, and 40-column previews
were visually inspected. The full workspace and CLI/session tests are outside this isolated TUI
run.

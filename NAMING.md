# Naming — Zenithal

Product name: **Zenithal**. Two words in prose and branding, one slug in code.

The precedent is `claude` → "Claude Code", `code` → "Visual Studio Code": the display name
carries the words, the binary stays short and typeable.

## Canonical names

| Context | Value |
|---|---|
| Product / brand / prose | **Zenithal** |
| GitHub repo | **`Hubris-tic/zenithal`** (fork of `openai/codex`) |
| CLI binary | **`zenithal`** |
| Config home | **`~/.zenithal/`** |
| Env prefix for the fork's own vars | **`ZENITHAL_*`** |
| Provider crate | **`zenithal-providers`** (lib `zenithal_providers`) |
| Crate dir in the workspace | **`codex-rs/zenithal-providers`** |
| Working tree here | **`zenithal/`** |

`zenithal` (bare) is reserved for identifiers: binary, config dir, crate prefix, short slugs.
Never for prose, never in a title, never on a landing page.

## Rename history

| Step | From | To |
|---|---|---|
| 1 | `Hubris-tic/codex` | `Hubris-tic/zenithal` |
| 2 | `Hubris-tic/zenithal` | **`Hubris-tic/zenithal`** |

Both old URLs return `301` and git still resolves them, so nothing that already cloned
breaks. Don't rename a third time without a reason — each hop adds a redirect and confuses
anyone who bookmarked the last one.

## What deliberately keeps upstream's names

Renaming these buys nothing and costs real merge pain:

| Kept as-is | Why |
|---|---|
| `openai/codex` (upstream remote, `ci/upstream-sync.yml`) | It is the actual upstream URL |
| `codex-rs/` (workspace root, 5,498 files) | Renaming the tree turns every rebase into a conflict festival |
| `codex-cli`, `codex_home`, `codex-mcp` (upstream crate/identifiers) | Same reason — only rename if you abandon upstream entirely |
| `gpt-5-codex` | OpenAI's model name, not our product |

**Rule:** rename the identity users see; leave upstream internals alone. That keeps the
Apache-2.0 derivation truthful and the fork maintainable.

## Applying it in your local clone

```bash
git clone https://github.com/Hubris-tic/zenithal.git zenithal
cd zenithal

# binary name
# codex-rs/cli/Cargo.toml
[[bin]]
name = "zenithal"          # was "codex"
path = "src/main.rs"

# provider crate
git mv codex-rs/providers codex-rs/zenithal-providers   # if already copied in

# config home: migrate, don't abandon
mv ~/.codex ~/.zenithal 2>/dev/null || true

cargo build --release -p zenithal
```

If you rename `CODEX_HOME` → `ZENITHAL_HOME`, do it behind a fallback read
(`ZENITHAL_HOME` → `CODEX_HOME` → `~/.codex`) in a single commit, so existing installs don't
lose their sessions on upgrade. Also print a one-line "moved to ~/.zenithal" notice rather
than silently switching.

## Two notes on the name itself

**Etymology.** "Zenithal" is the adjective for *zenith* — the point directly overhead, and by
extension the top of an arc. The visual identity is the other half of the name: a black hole,
the one object in the sky that nothing comes back out of, drawn with a red accretion ring.
That is a design story, not a translation, and there is nothing to over-claim.

One consequence worth stating: the earlier working name referenced the historical Nizari
Ismailis (the Alamut state, the "Assassins" legends). That is gone from every user-visible
string, and so is every derivative of it.

Three names survive because they were explicitly chosen and fixed:

| Name | What it is |
|---|---|
| **Strategos** | the boss agent: it plans, names and reviews. No tools. |
| **The Phalanx** | the collective of executors under Strategos |
| `xN-role` | the executors themselves, as short-hand system codes: `x0-core`, `x1-write`, `x2-check`, `x3-ship`, `x4-scan`, `x5-patch`, `x6-audit`, `x7-trace` |

An executor is never called "agent 2" and never gets a human name. It is `x1-write` or
`x7-trace`, chosen by Strategos from the catalogue in `brand/src/phalanx.rs` on the basis of
the work it was assigned, and the log prints the code with the role it stands for:

```
[strategos] name    slot-1     → x1-write · the primary logic generator
```

The same rule as the product name applies to code: bare `zenithal` in identifiers, the code
in the user-visible surface, no decoration.

**Trademark.** Apache-2.0 gives you the code, not the name. "Zenithal" is an ordinary English
word used by a few small companies; very unlikely to matter for an OSS dev tool, but do a
proper search before any commercial launch. Keep the `NOTICE` file and state that Zenithal is
a modified fork of `openai/codex`: that is both the licence obligation and the honest pitch.

## Verified after this rename

| Check | Result |
|---|---|
| `cargo build` | clean, 0 warnings |
| `cargo test` | **51 passed**, 0 failed |
| Crate | `zenithal-providers` (unchanged by branding) |
| GitHub | `Hubris-tic/zenithal`, fork of `openai/codex`, both old names `301` |
| `Zenithal Code` duplicates | none |
| Upstream refs preserved | `openai/codex`, `codex-rs`, `codex-cli`, `gpt-5-codex` intact |

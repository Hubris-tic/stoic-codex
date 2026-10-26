# Naming — Stoic Codex

Product name: **Stoic Codex**. Use the two-word form in prose and the hyphenated slug in
identifiers. This is the selected public name; do not revive earlier product names.

## Canonical names

| Context | Value |
|---|---|
| Product / brand / prose | **Stoic Codex** |
| GitHub repository | **`Hubris-tic/stoic-codex`** |
| CLI binary | **`stoic-codex`** |
| Config directory | **`~/.stoic-codex/`** |
| Environment prefix | **`STOIC_CODEX_*`** |
| Rust package prefix | **`stoic-codex-*`** (imports use `stoic_codex_*`) |
| Local checkout directory | User-chosen; `stoic-codex/` is recommended |

## Rename history

The current public rename is `Hubris-tic/zenithal` → **`Hubris-tic/stoic-codex`**.
GitHub's redirect from the previous repository name is expected; use the new URL for
documentation and remotes.

## Upgrade compatibility

On first CLI invocation, if `~/.stoic-codex/` is absent and `~/.zenithal/` exists, the CLI moves
the old directory to the new name and prints a notice. It never merges or overwrites: if both
folders exist, Stoic Codex uses the new folder, leaves the legacy folder untouched, and warns.

`STOIC_CODEX_HOME` is the new home-root override. `ZENITHAL_HOME` remains a lower-priority
compatibility alias; similarly, legacy `ZENITHAL_PROVIDER` and `ZENITHAL_TYPEDB_*` variables
remain fallbacks where applicable. Prefer the `STOIC_CODEX_*` names for new setups.

The TypeDB database identifier `zenithal_memory` and the secret-file directory
`~/.zenithal-keys/` are deliberately retained as persistence/secret compatibility identifiers;
renaming them would require a separate data migration or moving a credential. They are not
product display names. No credential file is copied, printed, or changed by this rename.

## Upstream names deliberately kept

This remains a fork. These identifiers describe upstream code or vendor products, not the
Stoic Codex brand, and are intentionally unchanged to avoid needless rebase conflicts:

| Kept as-is | Why |
|---|---|
| `openai/codex` | Actual upstream repository and Apache-2.0 attribution |
| `codex-rs/`, `codex-cli`, `codex_home`, `codex-mcp` | Upstream workspace and internal identifiers |
| `gpt-5-codex` | A vendor model name |

The Stoic Codex-specific crates, CLI, config path, URLs, branch namespace for new missions, and
user-visible strings use the new name. Do not rename upstream internals as part of this product
rebrand.

## Applying the name

```bash
git clone https://github.com/Hubris-tic/stoic-codex.git stoic-codex
cd stoic-codex
cargo build --release -p stoic-codex-cli
```

The binary is `stoic-codex`. The Rust packages are `stoic-codex-brand`,
`stoic-codex-providers`, `stoic-codex-memory`, `stoic-codex-agent`, `stoic-codex-ui-core`,
`stoic-codex-tui-views`, and `stoic-codex-cli`.

## Product story and attribution

“Stoic” is the chosen philosophical tone; “Codex” describes a collection of knowledge. Do not
claim a historical or etymological connection beyond that. The existing black-hole visual
direction remains unchanged.

Apache-2.0 gives the project the code, not exclusive rights to a name. “Codex” is also used in
OpenAI product and model names, so check trademark and naming-confusion risks before any
commercial launch. Keep the Apache notices and state plainly that Stoic Codex is a modified fork
of `openai/codex`.

The established mission names remain **Strategos**, **The Phalanx**, and the executor codes
`x0-core` … `x7-trace`; this rename does not change their roles.

## Validation status

The rename and transcript changes are in the working tree. This environment lacks `cargo` and
`rustc`, so compilation, tests, and regenerated TUI previews are pending and must not be described
as verified. Historical test counts in `CHECKLIST.md` predate these changes.

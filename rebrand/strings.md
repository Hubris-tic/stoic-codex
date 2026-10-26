# Upstream TUI rebrand inventory

**Scope clarification (2026-10-26):** this inventory tracks the upstream integration boundary
under `codex-rs/tui` and `codex-rs/cli`; it is not a status report about the standalone `tui/`
crate in this repository. The standalone TUI now displays “Stoic Codex” in its header, and its
cell-grid previews were adjusted and re-rasterized. Rebuilding those previews from the Rust
snapshot example remains pending because this environment has no Rust toolchain.

The counts below are a historical audit of upstream-facing files, not a fresh count against the
current public repository.

| Measurement | Count |
|---|---|
| String literals containing "codex" in `codex-rs/tui` + `codex-rs/cli` | 1,250 |
| …of those, shipped (outside `#[cfg(test)]` and `*_tests.rs`) | 354 |
| …of those, actually **user-visible** "Codex" (excludes model names, paths, ids) | **185** |
| URLs pointing at OpenAI infrastructure | **12** in `tui` alone, **24 supply-chain/misrouting** across `tui`+`cli` |
| …of the original 12, false positives (test-only) | 1 (`auth.openai.com` hyperlink fixture) |
| …fixed by patch 0001 | **all 24** |
| Features that would break or misbehave in a fork | **6** |

## Category A — must fix (functional, not cosmetic)

These are not about looks. Each one makes the product wrong or unsafe for a fork.

| File:line | Current | Problem | Fix |
|---|---|---|---|
| `tui/src/tooltips.rs:17` | `raw.githubusercontent.com/openai/codex/main/announcement_tip.toml` | The fork's TUI **fetches and displays OpenAI's announcements** as if they were ours | point at `ANNOUNCEMENT_TIP_URL`, or `None` to disable |
| `tui/src/tooltips.rs:22,25` | "Install it from https://chatgpt.com/codex…" | Ships users to a competitor's download page | rewrite or drop the tooltip |
| `tui/src/bottom_pane/feedback_view.rs:18` | `github.com/openai/codex/issues/new?template=3-cli.yml` | Users' bug reports land in a tracker we can't triage | `ISSUES_URL` |
| `tui/src/bottom_pane/memories_settings_view.rs:36` | `developers.openai.com/codex/memories` | Docs link for a feature that will not match our implementation | `DOCS_URL` |
| ~~`tui/src/custom_terminal.rs:1306`~~ | ~~`auth.openai.com/oauth/authorize…`~~ | **FALSE POSITIVE — corrected.** This is inside a `#[cfg(test)]` module (a hyperlink-wrapping fixture), not shipped code. No action. | — |
| `cli/src/desktop_app/mac.rs:10,12` | `persistent.oaistatic.com/codex-app-prod/Codex.dmg` | `stoic-codex app` would **download and install OpenAI's signed desktop app** | remove the subcommand or repoint |
| `cli/src/doctor/desktop/platform.rs:152,213` | `OpenAI.Codex_2p2nqsd0c76g0`, `OpenAI.Codex` | `doctor` verifies a package identity that will never be ours → false failures | remove these checks |
| `cli/src/doctor/security.rs:69,71` | signer team `2DC432GLL2` | Verifies Apple/Windows signatures from OpenAI's cert | drop the check |
| `cli/src/doctor/updates.rs:300` | `manifest.package_identity != "OpenAI.Codex"` | Auto-update identity check will always mismatch | remove/rewrite |
| `cli/src/state_db_recovery.rs:33,70,77` | "Codex couldn't start…" | Recovery text in the most stressful moment a user has | rebrand, keep the meaning |
| `tui/src/update_action.rs:47-70` | `npm i -g @openai/codex`, `brew --cask codex`, `curl chatgpt.com/codex/install.sh \| sh` | **Self-update replaces the fork with OpenAI's CLI** | **FIXED in patch 0001** |
| `tui/src/updates.rs:61,62` + `cli/src/doctor/updates.rs:36` | `api.github.com/repos/openai/codex/releases`, `cask/codex.json` | Version check compares you against upstream, so "update available" means OpenAI's release | **FIXED in patch 0001** |
| `tui/src/history_cell/notices.rs:59,76,84` | upstream install + release links | Tells users to install openai/codex | **FIXED in patch 0001** |
| `cli/src/doctor/updates.rs:392-396` | `npm install -g @openai/codex` | `doctor` printed an OpenAI install command as remediation | **FIXED in patch 0001** |

**Recommendation:** for the six *features* (desktop app, OAuth, doctor signature, updater,
announcement feed, marketplace), deleting them is better than renaming them. A renamed
installer that still downloads `Codex.dmg` is worse than no installer.

## Category B — user-visible copy (185 strings)

Grouped by where the user meets it, highest impact first:

| Surface | File(s) | Sample |
|---|---|---|
| Onboarding heading | `onboarding/welcome.rs:129` | `"Codex".bold()` — the first thing anyone sees |
| Session header | `history_cell/session.rs:47,386` | `"OpenAI Codex"`, `"OpenAI Codex (v{})"` |
| Composer placeholder | `custom_terminal.rs:1129`, `chat_composer.rs` | `"Ask Codex to do anything"` |
| Slash-command help | `slash_command.rs:95,98,110,117,149,159` | `"configure Codex preferences"`, `"exit Codex"` |
| Status line + terminal title | `status_line_setup.rs:195`, `title_setup.rs:102,126` | `"Codex application version"`, `"Codex app name"` |
| Permissions copy | `chatwidget/permission_popups.rs:449,460`, `permissions_menu.rs:15` | `"When Codex runs with full access…"` |
| IDE integration | `ide_context/ipc.rs:26,30,72,75,92,96,109,112,115` | `"Codex lost the IDE connection…"` |
| Recovery/errors | `app/daemon_menu.rs:58,111`, `daemon_recovery.rs:193` | `"Codex hasn't verified it manages this server."` |
| Model migration banner | `model_migration.rs:92` | `"Codex just got an upgrade…"` |
| Keymap setup | `keymap_setup/debug.rs:25,26,67`, `picker.rs:326` | `"Tip: Codex can only inspect keys…"` |
| Windows sandbox | `chatwidget/windows_sandbox_prompts.rs:38,42,160,164,212` | `"Set up the Codex agent sandbox…"` |
| Tool descriptions sent to the model | `dynamic_tools.rs:168–244` | `"Manage Codex tasks…"` — the model reads these, so they affect behaviour |
| Prompt scaffolding | `task_mentions.rs:35,268`, `user_prompt.rs:8`, `goal_files.rs:18` | `"## My request for Codex:"` |

That last row matters more than it looks: prompt scaffolding is *part of the protocol*
between our client and whatever model is behind `stoic-codex-providers`. Changing it changes
model behaviour, so it belongs in the same PR as a cassette/test update, not a bulk sed.

## Category C — leave alone

Do **not** rename these; each one is either upstream's property or a false positive.

| Pattern | Count | Why |
|---|---|---|
| `gpt-5-codex`, `gpt-5.2-codex` | many | OpenAI model names. Renaming them breaks routing |
| `codex_home`, `codex-rs`, `codex_cli`, `codex-mcp` | many | Internal identifiers; renaming taxes every future rebase |
| `openai/codex` in `LIMITATIONS`/`NOTICE`/attribution | 3 | Legally required attribution |
| `#[cfg(test)]` blocks and `*_tests.rs` | ~900 | Tests assert upstream behaviour; they will be updated by upstream itself |
| `.codex` in migration/legacy paths | few | Fallback reads that must keep working for existing installs |

## The right way to do this

1. `stoic-codex-brand` (already in `stoic-codex/brand/`) holds every string and URL. 5 tests, all green,
   including one that fails if any user-facing URL points at `openai.com`, `chatgpt.com`,
   `oaistatic.com` or `openai/codex`.
2. `rebrand/apply-rebrand.sh` applies Category A mechanically and idempotently, with
   `--dry-run` default and `--check` for CI.
3. Category B is a review task, not a script: 185 strings across 40 files, and the prompt
   scaffolding needs test updates alongside.
4. Copy the `stoic-codex-brand` test into `codex-rs/tui` as an integration test so a rebase can
   never silently reintroduce OpenAI URLs. That single test is what keeps the rebrand true
   six months from now.

## Effort

| Task | Estimate |
|---|---|
| Category A (6 features removed/repointed, 12 URLs) | 1–2 days |
| Category B mechanical (UI copy, 165 strings) | 1 day + review |
| Category B prompt scaffolding + test updates | 2 days |
| Brand crate wiring through TUI/CLI + the anti-regression test | 1 day |
| **Total** | **~5–6 days** of focused work, and then it stays done |

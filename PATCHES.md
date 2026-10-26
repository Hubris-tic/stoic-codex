# PATCHES — upstream divergence ledger (Stoic Codex)

Hard-fork rule: **every edit to an upstream file is recorded here**, one row per file,
with the reason and the seam it sits behind. When you rebase onto `upstream/main`, this
file is your review checklist: conflicts should only ever appear in rows below.

Fill the "applied" column with the commit sha. An empty ledger after a month means you
are carrying patches nobody remembers — that is the failure mode this file exists to
prevent.

| File (upstream path) | Change | Seam / trait | Risk | Applied |
|---|---|---|---|---|
| `codex-rs/Cargo.toml` | add `providers` to workspace members | — | low | _pending_ |
| `codex-rs/model-provider-info/src/model_provider_info.rs` | add `wire_adapter_id()` mapping | adapter id | low | _pending_ |
| `codex-rs/model-provider/src/capabilities.rs` | re-export `stoic_codex_providers::Capabilities` | capabilities | medium | _pending_ |
| `codex-rs/http-client/src/` | new `wire.rs` executing `WireRequest`, resolving `AuthSpec` | transport | medium | _pending_ |
| `codex-rs/config/src/` | `[model_providers.*].wire`, `[routing]` section | config schema | medium | _pending_ |
| `codex-rs/core/src/` | turn loop builds `CanonicalRequest`, consumes `CanonicalEvent` | turn loop | **high** | _pending_ |
| `codex-rs/protocol/src/` | (only if needed) `ResponseItem` mapping helpers | protocol | high | _pending_ |
| `codex-rs/tui/src/tooltips.rs` | announcement feed → our repo; app tooltips rewritten | `stoic_codex_brand` | medium | **0001-stoic-codex-tui-brand** |
| `codex-rs/tui/src/update_action.rs` | **self-update disabled** (installed OpenAI's CLI) | `from_install_context` → None | **high** | **0001** |
| `codex-rs/tui/src/{updates,update_prompt}.rs` | release checks → our repo | `stoic_codex_brand` | medium | **0001** |
| `codex-rs/cli/src/app_cmd.rs` | `app` disabled (installed Codex.dmg) | bail with message | **high** | **0001** |
| `codex-rs/cli/src/doctor/updates.rs` | latest-release + remediation strings → ours | `stoic_codex_brand` | medium | **0001** |
| `codex-rs/tui/tests/brand_urls.rs` | **new** anti-regression guard (hard/soft tiers) | n/a (new file) | low | **0001** |
| `codex-rs/tui/src/bottom_pane/feedback_view.rs` | issue URL → our tracker | `stoic_codex_brand::ISSUES_URL` | low | _pending_ |
| `codex-rs/tui/src/bottom_pane/memories_settings_view.rs` | docs URL + copy | `stoic_codex_brand::DOCS_URL` | low | _pending_ |
| `codex-rs/tui/src/onboarding/welcome.rs` | heading `"Codex"` → `Stoic Codex` | `stoic_codex_brand::NAME` | low | _pending_ |
| `codex-rs/tui/src/history_cell/session.rs` | session header `"OpenAI Codex (v{})"` | `stoic_codex_brand::version_header()` | low | _pending_ |
| `codex-rs/tui/src/custom_terminal.rs` | composer placeholder | `stoic_codex_brand::PLACEHOLDER` | low | _pending_ |
| `codex-rs/tui/src/slash_command.rs` | 4 `/help` strings | `stoic_codex_brand::NAME` | low | _pending_ |
| `codex-rs/tui/src/{status_line_setup,title_setup}.rs` | "Codex application version" / "app name" | `stoic_codex_brand::NAME` | low | _pending_ |
| `codex-rs/tui/src/model_migration.rs`, `app/daemon_menu.rs` | copy | `stoic_codex_brand::NAME` | low | _pending_ |
| `codex-rs/cli/Cargo.toml` + `cli/src/main.rs` | `[[bin]] name`, `bin_name` → `stoic-codex` | `stoic_codex_brand::BIN` | medium | _pending_ |
| `codex-rs/tui/src/custom_terminal.rs` (OAuth) | `auth.openai.com` flow — remove or gate | feature flag | **high** | _pending_ |
| `codex-rs/cli/src/desktop_app/*` | downloads `Codex.dmg` / `OpenAI.Codex` MSIX — **remove** | n/a (delete) | **high** | _pending_ |
| `codex-rs/cli/src/doctor/{security,desktop/platform,updates}.rs` | OpenAI signing identity / package id / updater — **remove** | n/a (delete) | **high** | _pending_ |
| `codex-rs/tui/src/dynamic_tools.rs` | tool descriptions sent to the model mention Codex | `stoic_codex_brand::NAME` | medium | _pending_ |
| `codex-rs/tui/src/{task_mentions,user_prompt,goal_files}.rs` | **prompt scaffolding** — part of the model protocol | `stoic_codex_brand::NAME` | **high** | _pending_ |

## TUI rebrand scope

Full inventory: `rebrand/strings.md`. Measured on `main`:

- 1,250 "codex" string literals in `tui` + `cli`; 354 shipped; **185 user-visible**.
- **12 URLs** point at OpenAI infrastructure (announcement feed, issue tracker, docs,
  OAuth, desktop-app downloads).
- **6 features** are wrong in a fork and should be deleted, not renamed: desktop-app
  installer, ChatGPT OAuth, doctor signature check, doctor package-identity check,
  auto-updater identity, and the upstream announcement feed.

Mechanical part: `rebrand/apply-rebrand.sh` (17 verified replacements, `--dry-run` default,
`--check` for CI, `--restore` from `.bak`). Human part: the 185 copy strings and the prompt
scaffolding, which needs tests updated alongside.

Patch files live in `stoic-codex/patches/`. Work log: `stoic-codex/tui/WORKLOG.md`.

## Rules

1. No upstream file is edited for a feature that could live in a new crate.
2. No upstream file is *restructured* — additions and trait implementations only.
3. If a patch exceeds ~150 lines in one file, the design is wrong: introduce a seam.
4. `upstream-sync` CI (see `README.md`) must be green before any release tag.
5. High-risk rows (core, protocol) get an integration test in the same PR.

## Policies that keep the fork viable

- Weekly `git rebase upstream/main`; fix conflicts the same week.
- Keep `Cargo.lock` committed and avoid version drift in patched crates.
- Keep the `NOTICE` file and state modifications (Apache-2.0 §4b). The license gives you
  the code, not the name: pick your own mark before publishing.

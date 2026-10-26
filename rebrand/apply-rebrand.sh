#!/usr/bin/env bash
# Apply the Stoic Codex rebrand to a checkout of the fork.
#
#   ./apply-rebrand.sh --dry-run [PATH]   # default: show what would change
#   ./apply-rebrand.sh --apply   [PATH]   # write changes (creates .bak files)
#   ./apply-rebrand.sh --check   [PATH]   # CI: exit 1 if any upstream URL remains
#   ./apply-rebrand.sh --restore [PATH]   # restore from .bak files
#
# Scope is deliberately narrow: Category A (URLs + features that are WRONG in a fork)
# plus the highest-impact UI strings. It never touches model names, tests, or internal
# identifiers, because those must keep matching upstream.
set -euo pipefail

MODE="dry-run"
ROOT="."
for a in "$@"; do
  case "$a" in
    --apply) MODE="apply" ;;
    --check) MODE="check" ;;
    --restore) MODE="restore" ;;
    --dry-run) MODE="dry-run" ;;
    -*) echo "unknown flag: $a" >&2; exit 64 ;;
    *) ROOT="$a" ;;
  esac
done
[ -d "$ROOT/codex-rs" ] || { echo "not a fork checkout (no codex-rs in $ROOT)" >&2; exit 1; }

TUI="$ROOT/codex-rs/tui/src"
CLI="$ROOT/codex-rs/cli/src"

# exact-string replacements: file|old|new
# Only fully-qualified URLs and unambiguous copy. No regex, no globs.
REPLACEMENTS=$(cat <<'EOF'
tui/src/tooltips.rs|https://raw.githubusercontent.com/openai/codex/main/announcement_tip.toml|https://raw.githubusercontent.com/Hubris-tic/stoic-codex/main/announcement_tip.toml
tui/src/bottom_pane/feedback_view.rs|https://github.com/openai/codex/issues/new?template=3-cli.yml|https://github.com/Hubris-tic/stoic-codex/issues/new
tui/src/bottom_pane/memories_settings_view.rs|https://developers.openai.com/codex/memories|https://github.com/Hubris-tic/stoic-codex/tree/main/docs/memories.md
tui/src/history_cell/session.rs|"OpenAI Codex".bold()|"Stoic Codex".bold()
tui/src/history_cell/session.rs|"OpenAI Codex (v{})"|"Stoic Codex (v{})"
tui/src/onboarding/welcome.rs|"Codex".bold()|"Stoic Codex".bold()
tui/src/bottom_pane/status_line_setup.rs|"Codex application version"|"Stoic Codex application version"
tui/src/bottom_pane/title_setup.rs|"Codex app name"|"Stoic Codex app name"
tui/src/bottom_pane/title_setup.rs|"Codex application version"|"Stoic Codex application version"
tui/src/slash_command.rs|configure Codex preferences|configure Stoic Codex preferences
tui/src/slash_command.rs|exit Codex|exit Stoic Codex
tui/src/slash_command.rs|log out of Codex|log out of Stoic Codex
tui/src/slash_command.rs|choose what Codex is allowed to do|choose what Stoic Codex is allowed to do
tui/src/model_migration.rs|Codex just got an upgrade.|Stoic Codex just got an upgrade.
tui/src/custom_terminal.rs|Ask Codex to do anything|Ask Stoic Codex to do anything
tui/src/bottom_pane/memories_settings_view.rs|Choose how Codex uses and creates memories|Choose how Stoic Codex uses and creates memories
tui/src/app/daemon_menu.rs|Run the Codex CLI to manage the daemon from this menu.|Run the stoic-codex CLI to manage the daemon from this menu.
EOF
)

# features that must be removed rather than renamed, in a fork
DISABLE=$(cat <<'EOF'
cli/src/desktop_app/mod.rs|OpenAI desktop-app installer subcommand
cli/src/doctor/desktop/platform.rs|OpenAI.Codex package-identity check
cli/src/doctor/security.rs|OpenAI code-signing identity check
EOF
)

changed=0
backup() { [ -f "$1.bak" ] || cp -p "$1" "$1.bak"; }

echo "== Stoic Codex rebrand ($MODE) in $ROOT =="
while IFS='|' read -r rel old new; do
  [ -n "${rel:-}" ] || continue
  f="$ROOT/codex-rs/$rel"
  [ -f "$f" ] || { echo "  skip (absent): $rel"; continue; }
  if grep -qF -- "$old" "$f"; then
    if [ "$MODE" = "apply" ]; then backup "$f"; python3 - "$f" "$old" "$new" <<'PY'
import sys, pathlib
p, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
path = pathlib.Path(p); s = path.read_text()
path.write_text(s.replace(old, new))
PY
      echo "  patched: $rel  <- $old"
    else
      echo "  would patch: $rel  <- $old"
    fi
    changed=$((changed+1))
  fi
done <<< "$REPLACEMENTS"

if [ "$MODE" = "check" ]; then
  echo
  echo "== check: any upstream OpenAI URLs left in shipped code? =="
  leftovers=$(grep -rn --include='*.rs' -E 'github\.com/openai|developers\.openai\.com|chatgpt\.com|oaistatic\.com|auth\.openai\.com' \
      "$TUI" "$CLI" 2>/dev/null | grep -vE '*_tests\.rs:|/tests/|^.*:[0-9]+:\s*//' || true)
  if [ -n "$leftovers" ]; then
    echo "$leftovers" | sed 's/^/  REMAINS: /'
    echo
    echo "Category A is not complete. See rebrand/strings.md."
    exit 1
  fi
  echo "  clean"
  exit 0
fi

if [ "$MODE" = "restore" ]; then
  find "$ROOT/codex-rs" -name '*.bak' -print0 | while IFS= read -r -d '' b; do
    mv -f "$b" "${b%.bak}"; echo "  restored: ${b#"$ROOT"/}"
  done
  exit 0
fi

echo
echo "replacements applied: $changed"
echo
echo "== still needing a human decision (do NOT blind-rename) =="
while IFS='|' read -r rel why; do
  [ -n "${rel:-}" ] || continue
  echo "  $rel  ->  $why"
done <<< "$DISABLE"
cat <<'TXT'

These are OpenAI-specific features. In a fork, renaming them is not enough:
  * desktop_app downloads Codex.dmg / OpenAI.Codex MSIX and installs OpenAI's signed app
  * doctor's signature + package-identity checks can never pass for us
  * auth.openai.com OAuth has no backend we control
Remove or gate them. See rebrand/strings.md, Category A.

Then verify on a machine that can build the 2.1M-line workspace (this sandbox cannot):
  cargo check -p codex-tui -p codex-cli
  cargo run -p codex-cli -- --version     # expect: Stoic Codex (vX.Y.Z)
TXT

#!/usr/bin/env bash
# PreToolUse(Bash) gate for `git commit`: blocks two mistakes this project
# has made repeatedly (see CLAUDE.md "Version bumps" and project memory
# feedback_always_bump_on_change / feedback_changelog_no_renames).
#
# 1. Any staged change under crates/padlock-*/src/, editors/vscode/src/, or
#    editors/vscode/package.json must come with a version bump (Cargo.toml +
#    editors/vscode/package.json's "version" line).
# 2. CHANGELOG.md must never lose a `## [X.Y.Z]` header that was already
#    committed — renaming/merging a past section instead of adding a new one.
set -euo pipefail

input="$(cat)"
command="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    data = {}
print(data.get("tool_input", {}).get("command", ""))
')"

# Only gate `git commit` invocations.
if ! printf '%s' "$command" | grep -qE '^[[:space:]]*git[[:space:]]+commit\b'; then
  exit 0
fi

cwd="$(printf '%s' "$input" | python3 -c '
import json, sys
try:
    data = json.load(sys.stdin)
except Exception:
    data = {}
print(data.get("cwd", "."))
')"
cd "$cwd" 2>/dev/null || true

# Not a git repo (shouldn't happen for this project, but don't block on it).
git rev-parse --show-toplevel >/dev/null 2>&1 || exit 0

staged_files="$(git diff --cached --name-only)"

# --- Rule 1: version-bump-on-change ---------------------------------------
if printf '%s\n' "$staged_files" | grep -qE '^crates/padlock-[^/]+/src/|^editors/vscode/src/|^editors/vscode/package\.json$'; then
  cargo_toml_changed=0
  git diff --cached --quiet -- Cargo.toml || cargo_toml_changed=1

  vscode_version_changed=0
  if git diff --cached -- editors/vscode/package.json | grep -qE '^\+.*"version":'; then
    vscode_version_changed=1
  fi

  if [ "$cargo_toml_changed" -eq 0 ] || [ "$vscode_version_changed" -eq 0 ]; then
    cat >&2 <<'EOF'
BLOCKED: source changes staged (crates/padlock-*/src/ or editors/vscode/src|package.json)
without a version bump.

This project's standing rule (CLAUDE.md "Version bumps", memory
feedback_always_bump_on_change): any change to a padlock crate or the VS
Code extension must bump the version — minor bump, every time, regardless
of publish status. Stage a bump to BOTH:
  - Cargo.toml ([workspace.package].version + [workspace.dependencies])
  - editors/vscode/package.json ("version" field)
before committing.
EOF
    exit 2
  fi
fi

# --- Rule 2: CHANGELOG no-rename -------------------------------------------
if ! git diff --cached --quiet -- CHANGELOG.md 2>/dev/null; then
  old_headers="$(git show HEAD:CHANGELOG.md 2>/dev/null | grep -oE '^## \[[0-9]+\.[0-9]+\.[0-9]+\]' | sort -u || true)"
  new_headers="$(git show :CHANGELOG.md 2>/dev/null | grep -oE '^## \[[0-9]+\.[0-9]+\.[0-9]+\]' | sort -u || true)"

  missing="$(comm -23 <(printf '%s\n' "$old_headers") <(printf '%s\n' "$new_headers") || true)"
  missing="$(printf '%s' "$missing" | sed '/^$/d')"

  if [ -n "$missing" ]; then
    {
      echo "BLOCKED: CHANGELOG.md is losing a previously-committed version header:"
      echo "$missing"
      cat <<'EOF'

This project's standing rule (memory feedback_changelog_no_renames): once a
"chore: bump version to X.Y.Z" commit has landed, that CHANGELOG section is
permanent. Never rename or merge it into a later version — add a new
section on top instead. If this rename already happened, reconstruct the
lost section from `git show <bump-commit>:CHANGELOG.md` rather than
collapsing it further.
EOF
    } >&2
    exit 2
  fi
fi

exit 0

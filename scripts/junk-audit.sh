#!/usr/bin/env bash
# Compares the compiled-in junk list against upstream .gitignore templates and
# prints what upstream carries that we do not.
#
# This exists because the list's failure mode is an entry nobody notices is
# absent: `cmake-build-debug` was missing for months, and no amount of reading
# the list would have revealed it. Only a diff against something external does.
#
# It NEVER edits the list. Every candidate is for a human to accept or reject,
# because a gitignore template and a tycho junk list answer different questions:
# gitignore excludes secrets and local state as well as build output, and those
# are exactly what tycho exists to back up. Blind import would drop `.env`.
#
# Usage: scripts/junk-audit.sh [--all]
#   --all   also list the entries we already carry, for review
set -euo pipefail

cd "$(dirname "$0")/.."
RULES=src/config/rules.rs
BASE=https://raw.githubusercontent.com/github/gitignore/main

# The ecosystems this project actually meets. Adding one here is how the audit
# widens; it is not meant to cover every language on earth.
TEMPLATES=(
  C C++ CMake Rust Node Python Java Gradle Maven Android Swift
  Objective-C Go Qt Unity
  Global/macOS Global/Windows Global/JetBrains Global/Xcode Global/VisualStudioCode
)

# Names that must never enter the list, whatever upstream says. A backup that
# skips these is not a backup - it is the failure this tool was built after.
DANGER='(^|[^a-z])(env|secret|secrets|credential|credentials|password|passwords|token|tokens|key|keys|id_rsa|id_ed25519|netrc|pypirc|npmrc|htpasswd|log|logs|dump|backup|data|config|settings|local)([^a-z]|$)|\.(env|log|pem|key|p12|pfx|keystore|jks|sqlite|db|csv|bak)$'

# Volume-level macOS and Windows droppings. Real, but they live at the root of a
# disk rather than inside a project, so carrying them would grow the list without
# ever matching anything a watch root contains.
VOLUME_CRUFT='^\.(Spotlight-V100|Trashes|fseventsd|DocumentRevisions-V100|TemporaryItems|apdisk|VolumeIcon|com\.apple|AppleD|Apple|PKInstall|MobileBackups|LSOverride|HFS|hotfiles|quota|vol|file|disk_label|localized|FBC|utmp)'

carried() {
  # The literals inside the junk! invocation. Loud rather than silent if the
  # shape of the list ever changes: the count check below fails the script.
  awk '/^pub const DEFAULT_JUNK/,/^};/' "$RULES" \
    | grep -o '"[^"]*"' | tr -d '"' | sort -u
}

CARRIED=$(carried)
COUNT=$(printf '%s\n' "$CARRIED" | grep -c . || true)
if [ "$COUNT" -lt 20 ]; then
  echo "error: parsed only $COUNT entries from $RULES - the list's shape changed" >&2
  echo "       fix the awk range in this script before trusting its output" >&2
  exit 1
fi

echo "tycho carries $COUNT entries"
echo "fetching upstream templates..."

FOUND=$(mktemp)
trap 'rm -f "$FOUND"' EXIT

for t in "${TEMPLATES[@]}"; do
  body=$(curl -fsSL "$BASE/$t.gitignore" 2>/dev/null) || { echo "  (skipped $t)" >&2; continue; }
  printf '%s\n' "$body" \
    | sed 's/\r$//' \
    | grep -v '^\s*#' \
    | grep -v '^\s*$' \
    | grep -v '^\s*!' \
    | sed 's|^/||; s|/$||' \
    | grep -v '/' \
    | grep -v '^\*\*' \
    | grep -vE '\[' \
    | grep -vE "$VOLUME_CRUFT" \
    | while read -r pat; do
        [ -z "$pat" ] && continue
        printf '%s\t%s\n' "$pat" "$t"
      done >> "$FOUND"
done

echo
echo "upstream has these, tycho does not:"
echo

sort -u "$FOUND" | awk -F'\t' '{ if (seen[$1]) { seen[$1] = seen[$1] ", " $2 } else { seen[$1] = $2; order[++n] = $1 } } END { for (i = 1; i <= n; i++) print order[i] "\t" seen[i in order ? order[i] : ""] }' \
  | while IFS=$'\t' read -r pat src; do
      printf '%s\n' "$CARRIED" | grep -qxF "$pat" && continue
      case "$pat" in *'*'*) kind=glob ;; *) kind=dir ;; esac
      note=""
      if printf '%s' "$pat" | grep -qiE "$DANGER"; then
        note="   REJECT: tycho exists to back this up"
      fi
      printf '  %-28s %-6s %s%s\n' "$pat" "$kind" "$src" "$note"
    done

echo
echo "nothing was changed. Add what you accept to DEFAULT_JUNK in $RULES,"
echo "and keep the bar: every entry must be a name a tool owns, never one a"
echo "person would choose for their own work."

if [ "${1:-}" = "--all" ]; then
  echo
  echo "currently carried:"
  printf '%s\n' "$CARRIED" | sed 's/^/  /'
fi

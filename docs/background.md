# Tycho background

Everything around the design that does not belong in the architecture docs: the history that
motivated it, the environment it runs in, and the exact state of the system it
replaces. Read alongside `docs/architecture/overview.md`.

## 1. Who and what this is for

- User: CoreEngineX, GitHub `CoreEngineX`, Toronto, Nova Scotia.
- Tycho is entity-agnostic. Nothing in its design depends on which CoreEngineX
  legal entity owns it; see the company records for that, which are not this
  document's to assert.
- Tycho backs up (a) the user's company data on macOS, and (b) a second person's
  company on their own machine, likely Windows, requirements not yet gathered.
  Multi-tenant means multiple config profiles; nothing is shared between them.

## 2. The system Tycho replaces

`~/Developer/CoreEngineX/org/handbook/scripts/backup-bundle.sh`, scheduled by
launchd label `com.coreenginex.docs-backup` (plist in `~/Library/LaunchAgents/`),
Sundays 12:00. Log: `~/Library/Logs/coreenginex-backup.log`. Verbatim:

```zsh
#!/bin/zsh
# Bundles company git repos (full history) into synced cloud folders.
# Destinations are auto-detected: any that exist receive a copy.
set -euo pipefail
setopt null_glob

REPOS=(
  "$HOME/Developer/CoreEngineX/org/handbook"
  "$HOME/Developer/CoreEngineX/org"
)
KEEP=8
STAMP=$(date +%Y%m%d-%H%M)
LOG="$HOME/Library/Logs/coreenginex-backup.log"

DESTS=()
[ -d "$HOME/Library/CloudStorage/OneDrive-Personal" ] && \
  DESTS+=("$HOME/Library/CloudStorage/OneDrive-Personal/CoreEngineX-Backups")
for gd in "$HOME/Library/CloudStorage"/GoogleDrive-*/"My Drive"; do
  [ -d "$gd" ] && DESTS+=("$gd/CoreEngineX-Backups")
done

if [ ${#DESTS[@]} -eq 0 ]; then
  echo "$(date '+%F %T') ERROR: no cloud destination found" >> "$LOG"
  exit 1
fi

TMP=$(mktemp -d)
for repo in "${REPOS[@]}"; do
  git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1 || continue
  name=$(basename "$repo")
  bundle="$TMP/$name-$STAMP.bundle"
  git -C "$repo" bundle create "$bundle" --all >> "$LOG" 2>&1
  git bundle verify "$bundle" >> "$LOG" 2>&1
  for dest in "${DESTS[@]}"; do
    mkdir -p "$dest"
    cp "$bundle" "$dest/"
    ls -t "$dest/$name-"*.bundle 2>/dev/null | tail -n +$((KEEP+1)) | while read -r old; do
      rm -f "$old"
    done
  done
  echo "$(date '+%F %T') OK: $name -> ${#DESTS[@]} destination(s) ($(du -h "$bundle" | cut -f1 | tr -d ' '))" >> "$LOG"
  rm -f "$bundle"
done
rmdir "$TMP"
```

**The bug, found 31-Jul-2026, deliberately left unfixed** (user decision
01-Aug-2026: Tycho will replace the script rather than patch it). Line 33's
`git bundle verify "$bundle"` has no `-C "$repo"`. `git bundle verify` needs a
repository for context. Manual runs from inside handbook worked; launchd runs
have a cwd that is not a repo, so verify fails with "error: need a repository to
verify a bundle" and `set -euo pipefail` aborts before any copy.

Evidence: three such errors in the log; `launchctl list` shows last exit status 1
for `com.coreenginex.docs-backup`, re-confirmed 01-Aug-2026; both cloud folders'
newest bundles are from a manual run on 22-Jul 18:17, so the scheduled 26-Jul run
left nothing. Every scheduled run covered by the log failed silently. The log does
not reach back to the job's installation, so "every run ever" is the likely reading
rather than the established one - what is established is that no bundle in either
destination carries a Sunday-12:00 mtime.

Consequence of leaving it unfixed: there are no working scheduled backups until
Tycho ships and is dogfooded - meaning the first release installed on this machine
and confirmed by a successful *scheduled* run, not a manual one. That raises the
urgency, and it makes the retirement plan a cutover from nothing rather than from a
working system.

Known gap in the script beyond the bug: `org`'s submodules (brand-assets,
toolkit, website, handbook) are captured only as gitlink pointers
in the org bundle. Only handbook gets a bundle of its own. The rest rely on
their GitHub remotes existing.

## 3. The war stories, which are the requirements

1. **Silent scheduled failure.** The line-33 bug above. Requirements it created:
   verification must run in a correct context; every failure must be loud
   (non-zero exit, red status, desktop notification); `doctor` and
   `status --check` make health one command; the integration suite carries a
   line-33 memorial test that runs bundle verify with cwd set to a non-repo.
2. **The Google Drive folder-sync deletion, July 2026.** A Drive live-sync
   misconfiguration deleted files under the repo. Git restored everything except
   `handbook/CLAUDE.md`, which was gitignored: the only unprotected file was
   the one git could not resurrect. Unnoticed for days, rebuilt from memory.
   Requirements: Tycho never consults .gitignore for capture decisions, and the
   repo-capture overlay exists precisely to hold uncommitted, untracked, and
   ignored files.
3. **The rm -rf and TRIM disaster, 18-Jun-2026.** `rm -rf` during a
   submodule-conversion task destroyed the local-only `ember` branch on
   a sibling project-ios, an entire v2.0 backend rebuild, committed locally, never
   pushed. Apple SSDs TRIM-zero freed blocks within seconds; Disk Drill, Data
   Rescue and PhotoRec all failed. Total loss. Requirements: local-only history
   is treated as already dead, so Tycho pushes to remotes every run.
4. **The org remote deletion.** `github.com/CoreEngineX/org` no longer exists and
   local org commits pile up unpushable. The cloud bundles are org's only
   off-site copy. Do not assume a GitHub remote exists for everything Tycho
   protects; folder remotes are the trust anchor.

## 4. Environment facts for the first machine

- macOS, Darwin 25.6.0, zsh, Apple Silicon. Git present. `/usr/bin/trash` exists
  and is mandated over recursive rm.
- Cloud mounts observed live, exact paths, re-confirmed 01-Aug-2026:
  - `~/Library/CloudStorage/OneDrive-Personal/` - backup destination
  - `~/Library/CloudStorage/GoogleDrive-Acct/My Drive/` -
    backup destination. The account is the backup account, not the user's
    personal gmail.
  - `~/Library/CloudStorage/OneDrive-Work/` - university account,
    never a backup target, never write there.
  - `a phone mount` - a phone mount, irrelevant.
- Scale calibration: handbook bundle ~7.7 MB, org bundle ~112 KB. The full
  dev tree `~/Developer/CoreEngineX` is low single-digit GB once junk dirs are
  excluded. For contrast, `a sibling tool` reports ~38 GB in the global cargo target
  dir alone, which is exactly the kind of thing the default ignores must catch.
- Repo layout: `~/Developer/CoreEngineX/{org,products}`. `org` contains
  submodules brand-assets, toolkit, handbook, website.
  `products/` holds standalone repos a sibling project, pager, spass-converter, and the
  platform repos inside those (a sibling project-ios, -android, -web, -docs, -img-core,
  pager-daemon, -docs, spass-core, -docs, -ios, -web) are submodules of them.
  Nested-repo capture is the normal case here, not an edge case.

## 5. Precedent code

- The old script above is the only in-house prior art for the job itself.
- `system-tools/daemons/a sibling daemon` is the reference for the local shape:
  crate layout, `scripts/ci-check.sh` shim over `cex ci-check --rust`, a launchd
  plist, an installer, a `docs/architecture/` doc.
- `a sibling crate` is the house reference Rust crate for clippy config and
  workspace conventions.
- `~/.claude/guidance/architecture.md` covers ports-and-adapters discipline.
  Tycho's one real port is the remote; resist trait-ifying anything with a single
  implementation.

## 6. Loose ends outside Tycho

- The one-line `-C` fix to the old script: offered, declined 01-Aug-2026. The old
  script stays broken by choice.
- `CoreEngineX/org`'s GitHub remote is missing and 8-plus local-only org commits
  cannot be pushed. Tycho's org coverage makes this less urgent, not resolved.
- Whether Google Drive live-folder-sync was disabled after the July incident was
  never explicitly confirmed. No recurrence observed since 16-Jul.

## 7. The name

Tycho Brahe (1546-1601), the Danish astronomer, spent roughly twenty-one
years at his observatory Uraniborg on the island of Hven (1576-1597) making
systematic naked-eye measurements of stellar and planetary positions, before
the telescope existed, at a precision nobody could match. When he died in
1601 his assistant Johannes Kepler inherited the observations and derived the
laws of planetary motion from them, particularly from the Mars data. Tycho's
own model of the solar system was wrong. The data was not. The records
outlived their maker and were faithful enough that someone else could
reconstruct deeper truth from them - which is exactly what a backup is for.

The etymology is commonly got backwards. Tyge is a Danish name of Old Norse
origin, usually traced to Tóki, a pet form of names built on the Þórr (Thor)
element. Renaissance humanists Latinising Danish names rendered Tyge as
Tycho, a spelling that happens to mirror the Greek name Τύχων (Tychon),
connected to τύχη (tyche, "fortune") and the verb τυγχάνω ("to hit the
mark"). So the "fortunate one who hits the mark" reading is a resemblance
acquired at the moment of Latinisation, not an inheritance from the Greek -
a coincidence, not a derivation.

The Moon's most prominent crater is named Tycho, and the supernova he
observed in 1572 - described in *De nova stella* (1573), the source of the
word "nova" - is called Tycho's Supernova.

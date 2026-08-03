# Build plan

Bottom-up, from primitives with no Tycho meaning to the CLI. Each layer is finished
and tested before the next begins, and each layer may only depend on layers below it.

This file is self-contained on purpose. It carries the decisions, exact invocations,
invariants and test vectors needed to build without re-reading the conversation that
produced them. The architecture docs are the contract; this is the order of work.

## Ground rules

- **Rust 1.97.1, edition 2024.** Single binary crate, no workspace.
- **`bash scripts/ci-check.sh` green before every commit.** fmt, the gated pedantic
  clippy set, tests, doc, audit. It currently fails with "could not find Cargo.toml",
  which is correct until layer 0 lands.
- **Cargo.lock is tracked** (binary crate). Dependency churn goes in its own commit,
  never folded into a logical change.
- Typed errors per module with `thiserror`; `anyhow` only in `main`.
- No `unwrap`/`expect` outside tests. `#[must_use]` on anything a caller must not drop.
- Newtypes at every module boundary - never a raw `String` or `PathBuf` crossing one.
- Sum types over optionals and flags, so illegal states do not compile.
- Comments only for what the code cannot say. No history narration.
- Commit and **push** at every meaningful step.
- **A dependency lands in the slice that first uses it**, in its own commit, so
  `cargo audit` watches only what is linked. The one permanent exclusion is a git
  library, per D7.
- **`#[ignore]` is not an escape hatch.** The gate runs `--run-ignored=all`.

### Open at slice 6: the `ignore` crate

`capture.md` section 3 specifies it, with `standard_filters(false)`,
`.hidden(false)` and `.follow_links(false)`. Decide it there rather than assume it,
because its filters are **opt-out**: the default `WalkBuilder` honours `.gitignore`,
which is the behaviour D9 forbids and the behaviour that lost `CLAUDE.md`. Weigh
that against a hand-rolled recursion, which the two walks may want anyway since
content stops at a repository boundary and discovery does not, and `symlink_metadata`
is needed regardless for `st_mode` classification.

## Decided, do not relitigate

These came out of the design audit (57 of 58 Critical and High findings confirmed by
adversarial verification). `docs/decisions.md` D13-D16 carries the reasoning.

| Decision | Why it is not negotiable |
|---|---|
| `hash-object -w --no-filters` | Without it `core.autocrlf` or a git-lfs clean filter silently alters stored content |
| `info/attributes` written at store init, and re-written after any mirror clone | A `.gitattributes` captured into the store's own tree makes `git archive` drop files and rewrite line endings, at exit 0 |
| Every store git invocation pins `-c` config | Nothing may be inherited from the user's global config |
| No `--prune` on the capture fetch, gc expiries pinned to `never` | Pruning orphans captured history; git's default then deletes it after two weeks |
| Push is `--atomic`, `+` on `refs/tycho/*` only | With `+` on heads, a second machine silently destroys the first's backup at exit 0 |
| Remote gets `receive.autogc=false`, `gc.auto=0` | On by default; it permanently prunes whatever a bad push orphaned |
| `--ignored=traditional`, not `matching` | `matching` collapses ignored directories, making per-file rule filtering impossible |
| `--no-optional-locks` on every source-repo git call | Plain `git status` rewrites `.git/index`, breaking the read-only invariant |
| Repository discovery recurses through repo boundaries | Otherwise every submodule is silently reduced to a gitlink |
| A root yielding zero entries fails the run | A TCC-denied root otherwise produces a green empty backup |
| Labels are `com.coreenginex.tycho.profile.<profile>` | A profile named `catchup` would otherwise collide with the catch-up agent |

---

## Layer 0 - primitives

No Tycho semantics. Pure, total, exhaustively unit-tested. Nothing here knows what a
backup is.

### 0.1 Validated newtypes

```rust
struct AbsPath(PathBuf);      // expanded, absolute, verified
struct ProfileName(String);   // [a-z0-9][a-z0-9-]*, not "catchup"
struct RootAlias(String);     // encodes to a valid single ref component
struct RemoteName(String);
struct BranchName(String);
struct RefName(String);       // full refname, check-ref-format validated
struct Oid([u8; 20]);         // width derived, never a hardcoded 40 or 41
```

Every one has a `Result`-returning constructor and no other way in. Expansion rules:
`~` to home; `$VAR` only from `{HOME, USER}`; an unset or empty variable is an error,
never an empty string; result must be absolute.

### 0.2 Encoders

Three distinct encodings, and conflating any two is a silent-corruption bug:

| Encoder | Used by | Rule |
|---|---|---|
| Percent | refname components | Encode every byte outside `[A-Za-z0-9._-]`, including `%`. Also encode a leading `.` and a `.lock` suffix |
| C-quote | `hash-object --stdin-paths` | Git's `quote_c_style`. Required because that command has no `-z` and a newline in a filename otherwise splits one path into two |
| NUL | `update-index -z --index-info` | Raw bytes, NUL-delimited. Without `-z`, a path starting with `"` is silently dequoted and any path with a `.git` component silently discarded |

Round-trip property tests, plus fixed vectors for newline, tab, quote, backslash,
non-UTF-8 bytes, leading dot, `.lock`, and a name that is `%` itself.

### 0.3 Ref collision detection

Case-fold and NFC-normalise a set of destination refnames and report duplicates. On
APFS, `Feature` and `feature` map to one file; git errors on first exposure but the
*next* fetch is silent and clobbers the captured tip.

**Test vectors for layer 0** are in `capture.md` section 8 (Hostile filenames,
Refnames rows).

---

## Layer 1 - process and filesystem primitives

Still no Tycho semantics. This layer exists so that no higher layer ever calls
`Command::new("git")` directly.

### 1.1 The git runner

One function every git call goes through. It pins config, applies a timeout, and
returns typed output.

```rust
fn git(cwd: &Path, args: &[&str]) -> Result<Output, GitError>
```

Always injected, before any caller-supplied argument:

```text
-c core.autocrlf=false -c core.eol=lf -c core.attributesFile=/dev/null
-c user.name=tycho -c user.email=tycho@localhost -c core.quotePath=false
```

The identity pin is not cosmetic: `commit-tree` inherits the user's identity and
hard-fails under `user.useConfigOnly=true`.

**Every child process runs under a timeout.** A FIFO or `/dev/zero` in a watched
directory blocks `hash-object` forever, and a hung run holding a lock is how backups
stop silently.

### 1.2 The streaming pipe helper

The one place concurrency is mandatory.

```rust
fn git_streaming(cwd: &Path, args: &[&str], input: impl Iterator<Item = Vec<u8>>)
    -> Result<Vec<Vec<u8>>, GitError>
```

Stdin writing and stdout reading happen on **separate threads**. Writing all input
and then reading fills the OS pipe buffer and deadlocks - measured at roughly 2,300
files with 145-byte paths, and the threshold scales with path length.

Test with more than 5,000 items. The small fixtures used everywhere else cannot reach
the threshold, which is exactly why this bug survived design review.

### 1.3 Lock and atomic write

```rust
fn try_lock(path: &Path) -> Result<LockGuard, Held>   // std File::try_lock, 1.89+
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()>   // temp + rename
```

**Never a blocking lock.** `run` reports "a run is already in progress since
`<time>`" and exits non-zero; `push` exits 0 and does nothing.

### 1.4 Filesystem classification

```rust
enum FileKind { Regular { exec: bool }, Symlink, Skip(SkipReason) }
```

Exhaustive over `st_mode`. Socket, FIFO, block and char device are `Skip`, not an
implicit remainder - the remainder is what hangs the run.

---

## Layer 2 - git plumbing, typed

Wraps layer 1 into operations that speak `Oid` and `RefName` rather than strings.
Still no notion of profiles or backups. Tested against real git in tempdirs, which is
a better double than a mock.

```rust
fn init_bare(path: &AbsPath) -> Result<Store>      // every setting from store.md s2
fn hash_object_batch(&self, paths: &[AbsPath]) -> Result<Vec<Oid>>
fn update_index(&self, entries: &[IndexEntry]) -> Result<()>
fn write_tree(&self) -> Result<Oid>
fn commit_tree(&self, tree: Oid, parent: Option<Oid>, msg: &str) -> Result<Oid>
fn update_ref(&self, name: &RefName, to: Oid) -> Result<()>
fn fetch_refs(&self, from: &AbsPath, spec: &Refspec) -> Result<()>
fn push(&self, to: &AbsPath, specs: &[Refspec], atomic: bool) -> Result<()>
fn ls_remote(&self, to: &AbsPath) -> Result<BTreeMap<RefName, Oid>>
fn for_each_ref(&self, prefix: &str) -> Result<BTreeMap<RefName, Oid>>
fn archive(&self, commit: Oid, paths: &[String], out: &Path) -> Result<()>
fn diff_tree(&self, from: Option<Oid>, to: Oid) -> Result<Vec<Change>>
fn cat_file_blob(&self, oid: Oid) -> Result<Vec<u8>>
```

`init_bare` writes, in this order:

```text
git init --bare --object-format=sha1 --shared=0600 -b main <store>
git -C <store> symbolic-ref HEAD refs/heads/main
git -C <store> config core.logAllRefUpdates always
git -C <store> config gc.pruneExpire never
git -C <store> config gc.reflogExpire never
git -C <store> config gc.reflogExpireUnreachable never
printf '* -text -diff -filter -ident -export-subst -export-ignore\n' > <store>/info/attributes
```

`hash_object_batch` owns the **short-read recovery protocol**: on fewer hashes than
paths, attribute the failure to `paths[hashes.len()]`, record it as a warning, and
restart from the next path, looping until exhausted. Invariant: *either every planned
path is hashed, or the ones that were not are named.*

**Byte-exactness test, layer 2's reason to exist:** round-trip a CRLF file, a file
matching an lfs `clean` pattern, an `ident` file, an `export-subst` file and an
`export-ignore`d file, under a maximally hostile `GIT_CONFIG_GLOBAL`, and `cmp` each
against the original. The design as originally written failed all five.

Set that variable with `Command::env` on the git child, never `std::env::set_var`.
nextest gives each test its own process so either works under the gate, but plain
`cargo test` runs them on threads of one process and the mutation would leak.

---

## Layer 3 - domain core, pure

No IO. This is the layer whose testability the whole module split defends.

### 3.1 Config

serde with `deny_unknown_fields`, `version` checked first so a newer config says so
rather than naming a key. Full validation table in `config.md` section 8 - every row
is a test.

### 3.2 The rule tree

The heart, and the thing an earlier draft specified twice contradictorily.

```rust
enum RuleKind { Junk, Glob, ExplicitPath }   // tier 1, 2, 3
enum Verdict { Capture, Skip }

fn resolve(&self, path: &RelPath) -> Verdict
```

Algorithm: evaluate every rule against the path and each ancestor; each match has a
depth; **deepest wins**; ties break by tier; two explicit path rules at equal depth is
a config error caught earlier.

**The eight-row truth table in `config.md` section 5 is the test suite.** Case 8 is
the one to get right: a glob matching a filename matches at the file's own depth,
which is deeper than any directory rule above it.

### 3.3 Domain types

```rust
enum Entry { Plain(PlainFile), Repo(RepoRoot) }
enum RepoHead { Branch { name: BranchName, sha: Oid }, Detached { sha: Oid }, Unborn }
enum Schedule { Daily { at: TimeOfDay }, Weekly { day: Weekday, at: TimeOfDay }, Every(Duration) }
enum RunOutcome { Ok(RunStats), OkPartial { stats: RunStats, lagging: Vec<RemoteLag> }, Failed(RunError) }
enum RemoteState { Unseen, Synced { at: Timestamp, head: Oid }, Behind { runs: u32, last_seen: Timestamp }, Failed { at: Timestamp, reason: FailureReason } }
```

---

## Layer 4 - engine

First layer that knows what a backup is. Composes layers 2 and 3.

| Module | Owns | Key invariant |
|---|---|---|
| `plan` | the two walks, classification, the sanity gate | A root yielding zero entries is `RunError` |
| `capture` | hashing, ref fetch, overlay, `REPO.txt` | No path with a `.git` component reaches the store tree |
| `store` | commit pipeline, history, restore | The backup ref advances only on a completed commit |
| `remote` | push, first-contact classification, verification | Verification compares the **full ref set**, not just heads |
| `state` | run records | Atomic rename, never a partial write |

Two walks, not one: **content** stops at a repository boundary, **discovery**
continues through it. Getting this wrong silently reduces every submodule to a
gitlink, and on this machine nearly everything is a submodule.

Post-`write-tree` reconciliation: count tree entries against the planned file count
and fail loudly on a shortfall. One check that catches the short-batch class, the
silently-discarded-path class, and future variants together.

---

## Layer 5 - shell and driving

`platform` (paths, plist generation, notifications, the launchd access probe),
`config_edit` (`toml_edit` in place), `cli` (clap, rendering, exit codes), `main`
(composition root).

The renderer's golden fixture is the fixed scenario in `cli.md` section 2 - Monday
2026-11-02 09:14, `coreenginex` weekly since 2026-08-02. Every example in the docs
derives from it, so the docs and the tests cannot drift.

That scenario's banner reads `tycho 1.0.0`. The fixture must interpolate
`env!("CARGO_PKG_VERSION")` rather than carry the literal, or it breaks on the first
version bump.

---

## Order of work

| # | Slice | Done when |
|---|---|---|
| 1 | Crate skeleton, module tree, the command set, the exit contract | `ci-check.sh` green |
| 2 | Layer 0 | Newtypes, three encoders, collision detection, hostile-name vectors passing |
| 3 | Layer 1 | Runner with pinned config and timeout; streaming helper passing a 5,000-item test; lock; atomic write |
| 4 | Layer 2 | Plumbing typed; the five-case byte-exactness round-trip passing under a hostile global config |
| 5 | Layer 3 | Config parse and validate; rule tree passing all eight truth-table rows |
| 6 | `plan` + `run --dry-run` | Correct plan for the real CoreEngineX tree, submodules found, junk excluded |
| 7 | `store` + `run` + `history` | Plain files committing, message rendering, retention test passing (`gc --prune=now` leaves a deleted branch readable) |
| 8 | `capture` repos | Overlay filtered, nested-repo and submodule cases, `REPO.txt` |
| 9 | `remote` + `status` | Atomic push, full-ref verification, state machine, two-machine rejection test |
| 10 | `restore` | Three-way path resolution, overlay type-safety, `--store` from a remote |
| 11 | `service`, `doctor`, notifications | Plists, overdue check, launchd access probe |

Windows is after all of it.

## The bar for done

From `docs/disaster-recovery.md` and the war stories - the old system cleared none of
these:

1. `tycho run coreenginex --dry-run` produces a correct plan for the real tree.
2. A real run pushes verified to both cloud folders.
3. `tycho restore --at <yesterday>` round-trips: contents byte-identical, a repo
   re-clones with full history, a gitignored file comes back.
4. Deliberately break a remote: `status --check` exits non-zero and a notification
   fires.
5. Deliberately deny access to a root: the run **fails** rather than committing empty.
6. Retire `backup-bundle.sh` only after a **scheduled** run succeeds - not a manual
   one. That is the bar the old system never cleared.

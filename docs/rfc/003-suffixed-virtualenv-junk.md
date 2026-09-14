# RFC 003 -- Suffixed virtualenvs in the junk list

| Field             | Value                                            |
|-------------------|--------------------------------------------------|
| **Status**        | `Locked`                                         |
| **Ticket**        | `(none)` -- Tycho is not tracked in Linear, see RFC 002 D4 |
| **Branch**        | `rfc/venv-suffix-junk`                           |
| **Contract docs** | `docs/architecture/config.md`                    |
| **Start date**    | `2026-09-14`                                     |
| **Updated**       | `2026-09-14`                                     |

---

## Summary

`DEFAULT_JUNK` names `.venv` and `venv` as exact components, so a second environment
named by suffix - `.venv-litert`, `venv-py312` - matches nothing and is captured. One
did: 25,186 `site-packages` files, 260 MB into the store in a single run. Two glob
entries, `.venv-*` and `venv-*`, alongside the existing `cmake-build-*`.

---

## Why this RFC, why now

`mllab` keeps two uv environments: `.venv` from `pyproject.toml`, and `.venv-litert`
from `requirements-litert.txt`. The first is caught by the junk list's exact name; the
second is not caught by anything. It is gitignored (`mllab/.gitignore:2`, `.venv-*/`),
which in this project means it is *more* likely to be captured, not less - invariant 5
says `.gitignore` never affects capture, and the overlay exists precisely to keep
gitignored files. So the project's own declaration that this is disposable was the
thing that guaranteed it got backed up.

The run on 2026-09-14 reported it in the terms the >100 MB growth notice was built for:

```text
captured     11 changed, 25,232 added, 5 deleted
written      in 22s                                               260 MB
grew         since the last run                                   260 MB
             ...site-packages/torch/bin/protoc                   4.01 MB
             ...include/ATen/RedispatchFunctions.h               2.20 MB
```

Store went 201 MB to 460 MB and the growth was pushed to `gdrive` before anyone looked.

**Why it can't wait:** a store rebuild is already scheduled to drop this content. Doing
it before the rule lands means re-capturing the same 260 MB on the next run, so the fix
gates the rebuild rather than merely being adjacent to it.

**The five-part test, applied.** Regenerable: `uv venv .venv-litert && uv pip install -r
requirements-litert.txt`. Generator backed up: uv is a tool, and the manifest is
committed. Inputs backed up: `pyproject.toml`, `uv.lock` and `requirements-litert.txt`
are all tracked in `mllab`. Affordable: a download. Not a secret, not unique human work.
Passes on all five.

---

## Scope

### In

- `".venv-*"` and `"venv-*"` in `DEFAULT_JUNK`'s glob group.
- `scripts/junk-audit.sh` run in the same sitting, per the standing rule for any junk
  list change.
- A test that the suffixed form is skipped and a lookalike source directory is not.

### Out (deferred)

- Marker-based detection, recognising a virtualenv by the `pyvenv.cfg` PEP 405 writes
  inside it rather than by name. It catches an environment named `myenv`, which no glob
  can. See Alternative A: it is the better idea and the wrong shape for this change.
- A local `mllab/.tycho/rules.toml` entry. Unnecessary once the glob lands, and the
  house standard puts a name a tool owns in `DEFAULT_JUNK` rather than in one project.
- The 260 MB already in history. Removed by the rebuild this RFC gates, not by the rule.

---

## Guide-level explanation

Nothing to call. The two entries join the glob group and every existing consumer picks
them up, because they go through the same matcher as `cmake-build-*`:

```rust
globs:
    "cmake-build-*",
    // uv and virtualenv name a second environment by suffix.
    ".venv-*", "venv-*",
```

Verify it the way the guidance says to verify any rule, which is the only reliable
check for content inside a repository:

```text
$ tycho rules explain -p cex ~/Developer/CoreEngineX/products/photoflick/mllab/.venv-litert
skip     .venv-*
         the built-in junk list
```

---

## Reference-level explanation

### Types / data shapes

No type changes. `Junk::Glob` already exists and `junk!` already takes a `globs:` group:

```rust
pub enum Junk {
    Name(&'static str),   // whole path component, compared for equality
    Glob(&'static str),   // pattern matched against one component
}
```

### Behaviour and state

A glob entry enters at `Tier::Junk` with `Origin::Junk`, the weakest of both
orderings, so any explicit path, config glob or local `.tycho` rule beats it **at equal
depth**.

Depth is where a glob differs from a name, and the difference was measured rather than
assumed. `compile` anchors a bare pattern as `**/<pattern>`, and globset's `*` spans `/`
unless `literal_separator` is set, which it is not. So `**/venv-*` matches
`.../venv-tools/main.py` as well as `.../venv-tools`: the glob re-matches at every depth
below the directory it named, and therefore **out-deepens a `reinclude` written above
it**. Measured:

| Rule | Path | Verdict |
|---|---|---|
| `reinclude A/lab/venv-tools` | `A/lab/venv-tools` | Capture, ExplicitPath, depth 5 |
| `reinclude A/lab/venv-tools` | `A/lab/venv-tools/main.py` | **Skip**, Junk, depth 6 |
| `+ reinclude .../main.py` | `A/lab/venv-tools/main.py` | Capture, ExplicitPath, depth 6 |
| `+ reinclude .../main.py` | `A/lab/venv-tools/other.py` | Skip, Junk, depth 6 |

**This is pre-existing and not introduced here** - `cmake-build-*` was tested alongside
and behaves identically, RuleId 71 against 73 in the same tree. For a junk glob the
escape hatch is therefore per file, not per directory. Naming a `Junk::Name` entry has
no such asymmetry, which is what row 5 of the truth table pins.

Both consumers are covered without new wiring, which is the reason to prefer a glob
here. `plan::walk_root` resolves directories through `RuleTree::descend`, and
`capture::expand` resolves overlay content through `RuleTree::captures` and
`may_contain_captures`. Both call the same matcher. `cli::rules::explain` reads out of
the same tree, so the verification command reports the truth rather than a second
opinion.

### Invariants

- The junk list stays purely lexical. No entry requires reading a file to decide.
- `names_are_not_patterns` still holds: these are declared as globs, not names, so the
  compile-time check that a `Name` carries no metacharacter is unaffected.
- A glob matches one component and cannot span `/`, so neither entry can reach a path
  that merely contains the text.

### Contracts (protocol / trait / interface)

`(none)` -- no trait or interface changes.

**Patterns.** None applies; this is a list entry. Naming a pattern here would be the
failure `~/.claude/guidance/patterns/` warns about rather than a use of it.

---

## Migration and rollout

`(not applicable)` for code. Operationally the rule only governs future runs, so the
260 MB already committed is removed by the store rebuild that follows this merge, in
the order the tycho guidance prescribes: clear the remote's `cex.git` first, then
rebuild locally and push, so no non-fast-forward rejection fires a false failure banner.

**Rollback:** delete the two entries. The next run re-captures the environment.

---

## Edge cases

| Case | Behaviour |
|------|-----------|
| `.venv-litert` | Skipped, `Tier::Junk` |
| `.venv`, `venv` | Skipped as before, by the existing exact names |
| A source directory named `venv-tools` | Skipped. Accepted cost: see Drawbacks |
| A file named `.venv-notes.md` | Skipped. A glob matches a component whatever its kind |
| `reinclude` naming the environment directory | Rescues that directory node only, not the files inside it |
| `reinclude` naming one file inside it | Rescues that file. A sibling it did not name stays skipped |
| A repo's own `.tycho/rules.toml` re-including it | Same asymmetry; it is a property of junk globs, not of origin |
| Environment nested inside a captured git repo | Covered. `capture::expand` consults the same tree |
| `use_default_ignores = false` | Neither entry applies, as with every junk entry |

---

## Privacy, security, and cost notes

- **Privacy:** `(none)`.
- **Security:** `(none)`. No new trust boundary; a virtualenv holds no credential that
  is not also in the manifest that built it.
- **Cost / performance:** two more patterns in an already-compiled `GlobSet`, matched
  per component. Not measurable against the directory walk that dominates.

---

## Drawbacks

- **A glob is a guess about names, and this one can eat source.** A directory genuinely
  called `venv-tools` holding hand-written code would be dropped silently. Judged
  acceptable because the prefix is strongly conventional, but it is the exact risk the
  list's own comment about `*.d` says to take seriously, and it is the reason
  Alternative A is better in principle.
- **It does not catch an environment named `myenv` or `env`.** The fix is conventional
  rather than complete, and the next oddly-named environment costs another entry.
- **The escape hatch is weaker than a name's, and this was found by testing the claim
  rather than asserting it.** A reinclude naming a real `venv-tools` directory rescues
  the directory but not its contents, because the glob re-matches deeper than the
  reinclude sits. Rescuing content means naming each file. Pre-existing for every junk
  glob, `cmake-build-*` included, but it makes the first drawback above materially worse
  than a `Junk::Name` entry would be, and it is the strongest argument on record for
  Alternative A.

---

## Alternatives considered

### A. Marker detection: a directory holding `pyvenv.cfg` is a virtualenv

Strictly better at the job. PEP 405 has `venv` and `virtualenv` write `pyvenv.cfg` at
the environment root, so it identifies one whatever it is called, with essentially no
false positives - better on the very criterion the `*.d` comment sets.

Rejected for this change, not on merit. The rule tree is a pure function over paths with
an eight-row truth table as its specification, and a marker probe is filesystem IO, so
it cannot live inside the tree without giving up the property the rule tests depend on.
It would need a new concept beside the tree, hand-wired into `plan::walk_root`,
`capture::expand` and `cli::rules::explain` - three call sites that a glob reaches for
free - plus a ruling on whether a `reinclude` can rescue a file inside a marked
directory. That is a feature with its own design questions, and attaching it to a store
rebuild would be the wrong way to introduce it. Worth its own RFC.

### B. A local `mllab/.tycho/rules.toml` entry

Smallest possible change and correctly scoped to the project that has the environment.
Rejected: the house standard puts a name a tool owns in `DEFAULT_JUNK` and reserves
local files for project-specific facts. A uv environment is not specific to `mllab`.

### C. Do nothing, and let the >100 MB growth notice catch each one

It did catch this one, which is the argument. Rejected: it caught it after 260 MB had
been committed and pushed, and the standing rule for this list is that whatever turns up
gets written down so it stops turning up.

---

## Prior art and related work

- **Our platform, on this exact question.** The junk list already solves the identical
  shape one group above: `cmake-build-*`, with the comment "CLion names the build
  directory after the CMake profile, so the suffix varies". A tool that names a variant
  by suffix is a case this list has met before and answered with a component glob. This
  RFC applies the existing answer rather than inventing one.
- **What the tool itself guarantees.** PEP 405 specifies `pyvenv.cfg` at the environment
  root, and `uv` writes it - confirmed on the environment in question. It guarantees a
  *marker*, and specifies nothing at all about the directory's **name**: the name is the
  user's argument to `uv venv`. So the glob leans on convention, and only the marker
  leans on a guarantee. That gap is the whole content of Alternative A and is stated
  here rather than glossed.
- **How adjacent ecosystems draw the same line.** GitHub's `Python.gitignore` ships
  exactly this pair of strategies side by side: literal `.venv`/`venv`/`env` entries
  plus a glob, rather than a marker-based mechanism, because gitignore has no way to
  express one. `mllab/.gitignore` independently arrived at `.venv-*/`.
- **The general problem is classifying regenerable build output**, and the field that
  owns it is build and packaging tooling. The two known solutions are exactly A and this
  RFC's: identify by name (cheap, lexical, guesses) or identify by marker (needs IO,
  exact). Build systems generally use names in ignore files and markers in tooling that
  can afford a stat, which is the same split being made here.

---

## Future possibilities

- Marker detection (Alternative A) becomes the natural follow-up, and this RFC is the
  record of why it was not folded in here.
- If it lands, these two globs can stay as a cheap pre-filter or be deleted; the marker
  subsumes them either way.

---

## Decisions (resolved)

### D1. Glob or marker

**Chosen:** glob (user, from a presented comparison). The junk list is deliberately
lexical, and adding a filesystem probe to it is a change of kind, not of degree - it
deserves its own RFC and should not ride along with a store rebuild. Rejected:
Alternative A, on shape rather than merit.

### D2. `DEFAULT_JUNK` or a local rule file

**Chosen:** `DEFAULT_JUNK` (author). A uv environment is a name a tool owns on any
machine, which is the documented test for this list. Rejected: Alternative B.

### D3. Accept that a real `venv-*` source directory would be dropped

**Chosen:** yes (author). The prefix is conventional enough that a collision is
unlikely, and the identical risk is already carried by `cmake-build-*`. Corrected during
implementation: the escape hatch is per file, not per directory, so a collision costs
more to work around than first written. Recorded in Drawbacks rather than assumed away.

---

## Open questions

`(none)`

---

## Files that will change

| File                                          | Change  | Note                                            |
|-----------------------------------------------|---------|-------------------------------------------------|
| `src/config/rules.rs`                         | edit    | two glob entries, with the comment naming the tool |
| `src/config/rules.rs`                         | edit    | test: suffixed form skipped, lookalike source noted |
| `docs/architecture/config.md`                 | edit    | the junk list's documented contents             |
| `docs/rfc/003-suffixed-virtualenv-junk.md`    | **NEW** | this RFC                                        |

---

## Verification checklist

### Automated

- [ ] `scripts/ci-check.sh` green, real exit code read outside a pipe
- [ ] A test asserts `.venv-litert` and `venv-py312` resolve to skip at `Tier::Junk`,
      failing without the change, and that the bare names still do
- [ ] A test pins the per-file escape hatch, so the asymmetry is documented in code
- [ ] `scripts/junk-audit.sh` run and its answer read, per the standing rule

### Manual

- [ ] `tycho rules explain -p cex <mllab>/.venv-litert` reports skip by `.venv-*`
- [ ] `tycho rules explain -p cex <mllab>/.venv` still reports skip, by the exact name
- [ ] After the rebuild, the store holds zero `site-packages` paths
- [ ] After the rebuild, the store is back near its pre-incident size
- [ ] `tycho doctor` clean for `cex` apart from known-red `ghost`

---

## Sources

- [PEP 405 -- Python Virtual Environments](https://peps.python.org/pep-0405/) -- specifies `pyvenv.cfg` at the environment root, and specifies nothing about the directory name
- [GitHub `Python.gitignore`](https://github.com/github/gitignore/blob/main/Python.gitignore) -- literal names plus a glob, the adjacent-ecosystem cross-check

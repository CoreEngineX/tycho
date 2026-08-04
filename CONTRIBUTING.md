# Contributing

## The gate

```text
bash scripts/ci-check.sh
```

fmt, clippy under `-D warnings` with a pedantic set gated on, tests, doctests, docs
and a dependency audit. Green before every commit, no exceptions. The script runs the
checks directly if you have nothing but a Rust toolchain; `cargo install
cargo-nextest` is worth doing first, because the suite runs each test in its own
process and a few of them mutate process-global state.

`#[ignore]` is not an escape hatch. The gate runs `--run-ignored=all`.

## What a change has to carry

**The document changes in the same commit as the behaviour.** `docs/architecture/`
is the contract and this project has been burned repeatedly by prose that described
something the code stopped doing. If a change alters what a backup contains, how it
is written, or how it comes back, the matching section is part of the change.

**A claim in a comment or a doc is measured, not reasoned.** Several of the sharpest
bugs found here came from text that was carefully argued and wrong: an
`info/attributes` mechanism blamed for line endings it had nothing to do with, a
recovery procedure genuinely executed against the one repository whose particulars
made it pass. If you write down what git or the filesystem does, run it first and
say so.

**Tests prove they can fail.** A test that passes for the wrong reason is worse than
none. Where a mechanism is under test, run the case twice - once with it and once
without - or show the failure some other way. `tests/byte_exactness.rs` is the model.

## Style

- No `unwrap` or `expect` outside tests. Typed errors per module with `thiserror`.
- Sum types over optionals and flags, so illegal states do not compile.
- Comments say what the code cannot. No history narration - no `previously`, no
  `now`, no `changed from`. Default to no comment.
- `unsafe_code = "forbid"` stays. If a platform API appears to need `unsafe`, that is
  a discussion, not a lint to relax.
- A module may only depend on modules in a lower layer. `src/lib.rs` carries the
  table, and in a single crate the only enforcement is that an import is visible.

## Platform work

macOS and Windows are both supported and both are expected to keep working. The
crate refuses to build anywhere else with a message saying so rather than a pile of
unresolved imports.

`platform::scheduler` is a `#[cfg]`-selected module, and a private `contract` module
asserts both backends present the same surface - so a signature that drifts is a
compile error on whichever host builds, rather than a discovery on the other machine.
Keep it that way.

**Do not write platform code you have not run.** This project has twice shipped
something that compiled, read correctly, and was wrong. Where you genuinely cannot
run it, leave it unwritten behind a typed error and say so; `platform::notify` shows
the pattern.

## Reporting a bug

What you ran, what happened, what you expected, and the output of `tycho doctor`.
If it involves a remote or an external drive, the filesystem matters - say which.

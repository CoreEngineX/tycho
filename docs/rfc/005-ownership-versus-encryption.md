# RFC 005 -- `noowners` is not "unencrypted", and `doctor` should stop saying it is

| Field             | Value                                                         |
| ----------------- | ------------------------------------------------------------- |
| **Status**        | `Draft`                                                       |
| **Ticket**        | `(none)` -- Tycho is not tracked in Linear, see RFC 002 D4    |
| **Branch**        | `rfc/005-ownership-versus-encryption`                         |
| **Contract docs** | `docs/architecture/remotes.md`, `docs/architecture/config.md` |
| **Start date**    | `2026-10-10`                                                  |
| **Updated**       | `2026-10-10`                                                  |

---

## Summary

`doctor` tells the owner that an encrypted APFS card is "readable by anyone with the disk". It is
not: the card is FileVault-encrypted, and a thief gets ciphertext. The row fires because
`records_ownership` asks whether the **mount** ignores ownership, and macOS mounts every external
volume `noowners` whatever the filesystem - so the check is right about what it measures and the
sentence drawn from it is false. This RFC gives the privacy question its own predicate, one that
asks whether the bytes are encrypted before claiming they are exposed, and leaves git's quite
separate ownership question alone.

---

## Why this RFC, why now

Measured on this machine, 2026-10-10, on the `ghost-sd` remote:

```text
$ mount | grep CEX-SD64
/dev/disk17s1 on /Volumes/CEX-SD64 (apfs, local, nodev, nosuid, journaled, noowners)

$ diskutil info /Volumes/CEX-SD64
File System Personality:   APFS
FileVault:                 Yes

$ tycho doctor
  ghost-sd privacy      warn    /Volumes/CEX-SD64/tycho-backups records no ownership, so this
                                backup is readable by anyone with the disk; encrypt the volume
                                or accept it
```

The advice is already taken. The volume **is** encrypted, and the row still says to encrypt it.

**Two separate errors in one sentence.** First, "records no ownership" describes a filesystem, and
what was measured is a mount option. `src/sys/volume.rs:96` is `ignores_ownership(path).map(|ignores|
!ignores)`, and `exposure`'s own comment thirty lines above already states the consequence: _"`noowners`
is what Disk Utility's 'Ignore ownership on this volume' sets, and it is the default for removable
volumes whatever the filesystem - so an HFS+ or APFS external drive reaches it too."_ The codebase
knows; the message does not. Second, "readable by anyone with the disk" is a claim about encryption,
and nothing in the check looks at encryption at all.

**Why it can't wait.** This project's own stated rule is that a row red on every machine is a row
people learn to skim, and it has twice chosen behaviour on that basis: the sidecar sweep runs rather
than reporting, and the Spotlight check fires only on an unjournaled volume. Because `noowners` is the
macOS default for removable volumes, this row is red on **every external drive anyone will ever
attach**, encrypted or not. It is the exact failure mode the project designs against, sitting in the
project. And it is not merely noisy: it tells an owner who has already encrypted the volume that their
backup is exposed, which is worse than silence, because it spends trust on a false alarm.

**What is still true, and the fix must keep saying it.** While that card is mounted and unlocked,
ownership is ignored, so any account or process on this Mac reads the backup as owner. That is a real
and much narrower statement than the one being printed, and on a single-user machine it is close to
nothing.

---

## Scope

### In

- A privacy predicate that distinguishes "ownership is not enforced" from "the bytes are exposed", as
  a sum type rather than a second bool.
- Encryption detected from `diskutil info -plist` on macOS; unknown elsewhere, and unknown never reads
  as safe.
- `doctor`'s privacy row re-worded, and silent-or-green when the volume is encrypted.
- `cli/remote.rs`'s ownership error re-worded to name the mount, not the filesystem.
- `remotes.md` and `config.md` corrected where they state the filesystem claim.

### Out (deferred)

- Renaming `records_ownership`. The name is wrong on Unix but the predicate is right for git's
  question, and a rename touches both platform arms and every caller; see Alternative E.
- Changing when `--trust-ownership` is required. Git's refusal is driven by `noowners` and is correct
  as it stands; this RFC does not touch the store's own refusal in `git/repo.rs` either.
- Teaching `doctor` to offer to run `diskutil enableOwnership`. It needs root, and it is a decision
  about the disk rather than a side effect of a health check - the same reasoning the Spotlight
  remedy already uses.

---

## Guide-level explanation

Nothing new to call, and one row changes. On the encrypted card:

```text
  ghost-sd              ok      all refs present, verified
  ghost-sd privacy      ok      ownership ignored, volume encrypted
```

On a plain exFAT stick, the warning survives and says something true:

```text
  ghost privacy         warn    /Volumes/STICK/tycho ignores ownership and is not encrypted, so
                                this backup is readable by anyone who has the disk; encrypt the
                                volume or accept it
```

And where the platform cannot answer, it says so rather than guessing either way:

```text
  nas privacy           warn    /mnt/nas ignores ownership and encryption cannot be checked here
```

---

## Reference-level explanation

### Types / data shapes

The three outcomes are different in kind, so they are cases rather than a pair of bools. A
`(bool, bool)` would admit "enforced and exposed", which cannot happen.

```rust
/// What protects a remote folder's bytes from someone who is not its owner.
pub enum Protection {
    /// The mount enforces ownership, so the file modes mean what they say.
    Enforced,
    /// Ownership ignored, but the volume is encrypted at rest. `unlocked` is the
    /// state right now, not a property of the volume.
    EncryptedAtRest { unlocked: bool },
    /// Ownership ignored and nothing encrypts the bytes.
    Exposed,
    /// The platform could not be asked. Never treated as safe.
    Unknown(String),
}

/// Reads the mount, then asks the platform about encryption only if it has to.
pub fn protection(path: &Path) -> Result<Protection, VolumeError>;
```

### Behaviour and state

`protection` short-circuits: if the mount enforces ownership it is `Enforced` and nothing else runs,
so the boot volume pays nothing. Only a `noowners` mount reaches the encryption probe.

On macOS the probe is `diskutil info -plist <mount point>`, and the discriminating key was measured
rather than assumed:

| Volume                            | `Encryption` | `EncryptionThisVolumeProper` | `FileVault` | `Locked` |
| --------------------------------- | ------------ | ---------------------------- | ----------- | -------- |
| `/Volumes/CEX-SD64` (the SD card) | true         | **true**                     | true        | false    |
| `/` (the boot volume)             | true         | **false**                    | true        | false    |

`EncryptionThisVolumeProper` is the one that means "this volume's own data is encrypted"; `Encryption`
and `FileVault` are both true for the boot volume as well, so neither discriminates. The probe reads
`EncryptionThisVolumeProper` for the verdict and `Locked` for the `unlocked` field.

On any other Unix there is no equivalent to ask, so the arm returns
`Unknown("encryption cannot be checked on this platform")`. On Windows the existing DACL answer already
distinguishes a filesystem with no access control, so that arm maps `NO_ACCESS_CONTROL` to `Exposed`
and anything else to `Enforced`.

### Invariants

- `Unknown` never suppresses the warning. An unanswerable question is not a safe answer.
- `EncryptedAtRest` is a claim about the disk at rest, never about the mounted volume. The row's text
  must not imply the data is unreadable while the volume is mounted and unlocked.
- The encryption probe runs only on a `noowners` mount, so no command is spawned for the common case.
- Git's ownership question is untouched: `records_ownership` keeps its callers, its meaning and its
  `--trust-ownership` consequence.

### Contracts (protocol / trait / interface)

`(none)` -- no trait changes. `protection` is a free function beside `records_ownership`.

**Patterns.** The relevant discipline is not a Gang-of-Four pattern but the house rule on making
illegal states unrepresentable: one sum type with four cases, so "enforced but exposed" and "encrypted
but no mount answer" cannot be constructed. Naming a pattern here would be the failure the catalogue
warns about.

---

## Migration and rollout

`(not applicable)` -- no persisted state, no config key, no schema. The only observable change is the
text and colour of one `doctor` row.

**Rollback:** revert. The row returns to warning on every external volume.

---

## Edge cases

| Case                                                   | Behaviour                                                                                                       |
| ------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------- |
| Encrypted APFS, mounted, unlocked (today's `ghost-sd`) | `EncryptedAtRest { unlocked: true }`, row green                                                                 |
| Encrypted APFS, present but locked                     | `EncryptedAtRest { unlocked: false }`; the remote is unreachable anyway, so the push path decides, not this row |
| Plain exFAT stick                                      | `Exposed`, row warns with the corrected text                                                                    |
| APFS external, not encrypted                           | `Exposed`. Correct: ownership ignored and nothing encrypts it                                                   |
| Boot volume                                            | `Enforced` before any probe runs; no `diskutil` spawned                                                         |
| `diskutil` missing, slow or failing                    | `Unknown`, row warns and names the failure                                                                      |
| Linux or BSD remote folder on a `noowners`-ish mount   | `Unknown`, row warns                                                                                            |
| Windows exFAT remote                                   | `Exposed` via the existing `NO_ACCESS_CONTROL` answer                                                           |
| Volume disappears between the mount read and the probe | `Unknown`; the remote's own reachability check is what reports the real problem                                 |
| A network mount that ignores ownership                 | `Unknown` on macOS unless `diskutil` recognises it; warns, which is right for a share                           |

---

## Privacy, security, and cost notes

- **Privacy:** `(none)` -- no new data is read beyond volume metadata already on the machine.
- **Security:** this RFC makes a warning quieter, so the risk is under-warning. Two guards: `Unknown`
  never counts as safe, and the green row states the residual exposure in words rather than implying
  there is none. The genuinely dangerous case - an unencrypted removable volume - still warns, and is
  the case the row was written for.
- **Cost:** one `diskutil` subprocess per remote per `doctor` run, and only on a `noowners` mount, so
  zero on the boot volume. Comparable to the `mount` call the check already pays.

---

## Drawbacks

- **A quieter row can lull.** "Encrypted" is about a stolen card, not a mounted one, and someone could
  read the green row as "nothing can read this". The wording is the whole mitigation, which is thin.
- **It trusts `diskutil`'s text output.** A plist key is a stabler contract than scraped prose, but it
  is still not a documented API, and Apple can rename it. A wrong parse would read as `Unknown` and
  warn, which is the safe direction, but a silent reshape to a key that parses as `false` would warn
  where it should not.
- **It adds a platform-specific probe to a module that was almost platform-neutral.** `volume.rs`
  already has `#[cfg]` arms, so this is more of the same rather than new in kind, but it is more.
- **The exFAT arm cannot be exercised today.** No exFAT volume is mounted on this machine, so the
  `Exposed` path is reachable only by unit test until one is attached; see Verification.

---

## Alternatives considered

### A. Reword the message, detect nothing

One-line change: say "ignores ownership" instead of "records no ownership", drop the "readable by
anyone with the disk" clause. Honest and cheap. Rejected as insufficient: the row would still be amber
on every external drive on every machine forever, which is the skim problem this project explicitly
designs against, and the owner would still be told to encrypt a volume they have encrypted.

### B. Suppress the privacy row for all removable volumes

Removes the noise completely. Rejected: an unencrypted exFAT card in a bag is exactly the case the row
exists for, and this would delete the warning precisely where it is true.

### C. A per-remote `accept_exposure = true` config key

Lets the owner silence it per remote. Rejected: it asks a person to assert something the machine can
measure, and a config key is forever. Worth revisiting only if the probe proves unreliable.

### D. `NSURLVolumeIsEncryptedKey`, the documented API

The obvious first choice, and it does not work. Apple's own developer forums record that it returns
true for APFS volumes whether or not they are encrypted, because APFS encryption differs from the
older model the key was designed for. **This absence of a usable API is the finding that forces
`diskutil`**, and it is the reason this RFC parses a plist rather than calling a documented function.

### E. Rename `records_ownership` to say what it measures

Tempting, since the name is the root of the confusion. Deferred rather than rejected: the predicate is
correct for git's question, the name was deliberately chosen so the Unix and Windows arms carry one
name, and a rename touches both arms and three call sites for no behavioural gain. It belongs in its
own change where it is the point, not smuggled into a privacy fix.

---

## Prior art and related work

- **Our platform, on this exact question.** macOS mounts an external filesystem `noowners` by default
  unless `/var/db/volinfo.database` carries an entry for that volume UUID, which is what Finder's
  "Ignore ownership on this volume" toggles and what `vsdbutil` used to write - now deprecated in
  favour of `diskutil enableOwnership`. So `noowners` is the **norm for removable media, not an
  anomaly**, which is precisely why the current row fires universally. Apple documents the toggle and
  the default; it does not document `noowners` as implying anything about confidentiality, and that
  silence is the gap the current message fills with an invention of its own.
- **What the vendor guarantees, and what it does not.** `NSURLVolumeIsEncryptedKey` is the documented
  way to ask, and Apple's forums record that it answers `true` for every APFS volume regardless of
  encryption. **The absence of a trustworthy documented API is itself the finding**, and it is why this
  design reads `diskutil info -plist` instead. The keys it reads are measured on this machine, not
  taken from documentation, and the table above records both the discriminating key and the two that
  look discriminating and are not.
- **How adjacent platforms draw the same line.** Windows has no `noowners`: a volume either supports
  access control or reports `NO_ACCESS_CONTROL`, which genuinely is a filesystem property. The existing
  Windows arm therefore already answers the question the Unix message _claims_ to answer, which is how
  one shared name came to carry two different meanings. Linux has no per-mount ownership-ignoring flag
  of this shape at all, so there is nothing to port and the honest answer there is `Unknown`.
- **The general problem is a conflated predicate** - one boolean serving two questions whose answers
  coincide on the platform it was written for and diverge elsewhere. The standard remedy is the one
  taken here: split the predicate by the question rather than by the mechanism, and let each caller
  name what it actually needs.

---

## Future possibilities

- If `protection` proves sound, the store's own refusal in `git/repo.rs` could use it to distinguish
  "refuse, this is exposed" from "refuse, git cannot verify ownership", which today are one message.
- A `doctor --deep` arm could verify the claim empirically by writing a file as another user, rather
  than trusting the mount flag.

---

## Decisions (resolved)

### D1. Fix the message, or fix the predicate

**Chosen:** give the privacy question its own predicate (author). Rewording alone leaves the row amber
on every external drive forever, which is the failure the project's own doc argues against twice.
Rejected: Alternative A.

### D2. Sum type rather than two booleans

**Chosen:** `Protection` with four cases (author, following the house rule). `(ignores_ownership,
is_encrypted)` admits a pair that cannot occur and forces every caller to re-derive the meaning.

### D3. `Unknown` warns

**Chosen:** yes (author). The change makes a security warning quieter, so every unanswerable case must
fall on the loud side or the quietness is unbounded.

### D4. Leave `records_ownership` and git's behaviour alone

**Chosen:** yes (author). It is correct for git's question; only the privacy reader was wrong.
Rejected for now: Alternative E.

### D5. On an encrypted volume, a green row rather than silence

**Chosen:** the green row (user, 2026-10-10). A check nobody can see having run is indistinguishable
from a check that never ran, which is the same problem the warning exists to solve. Was Q1.

---

## Open questions

`(none)` -- Q1 resolved, see D5.

---

## Files that will change

| File                                          | Change  | Note                                                                       |
| --------------------------------------------- | ------- | -------------------------------------------------------------------------- |
| `src/sys/volume.rs`                           | edit    | `Protection`, `protection()`, the macOS `diskutil` probe and its parser    |
| `src/sys/volume.rs`                           | edit    | tests: the plist parser against captured output for both volumes           |
| `src/doctor.rs`                               | edit    | the privacy check consumes `Protection`; new wording                       |
| `src/cli/remote.rs`                           | edit    | the ownership error names the mount, not the filesystem                    |
| `docs/architecture/remotes.md`                | edit    | what the privacy row means, and that `noowners` is the removable default   |
| `docs/architecture/config.md`                 | edit    | the `trust_ownership` row's "filesystem that records no ownership" wording |
| `docs/rfc/005-ownership-versus-encryption.md` | **NEW** | this RFC                                                                   |

---

## Verification checklist

### Automated

- [ ] `scripts/ci-check.sh` green, real exit code read outside a pipe
- [ ] The plist parser is tested against captured `diskutil info -plist` output for an encrypted
      external volume and for the boot volume, and returns different verdicts for them
- [ ] A test asserts `Unknown` warns rather than passing
- [ ] A test asserts an `Enforced` mount never reaches the probe

### Manual

- [ ] `tycho doctor` on `ghost-sd` (encrypted APFS, `noowners`) no longer warns, and says what Q1 settles
- [ ] `tycho doctor` with an **unencrypted exFAT** volume attached still warns, with the new wording.
      Cannot be done today: no exFAT volume is mounted on this machine
- [ ] No `diskutil` process is spawned when every remote is on the boot volume
- [ ] `tycho remote add` against a `noowners` volume still demands `--trust-ownership`, unchanged

---

## Sources

- [`vsdbutil` and the volume status database](https://www.oreilly.com/library/view/mac-os-x/0596009437/re288.html) -- external filesystems mount `noowners` unless `/var/db/volinfo.database` carries the volume, and the Finder toggle is the same switch
- [Get Encryption Status for Volume, Apple Developer Forums](https://developer.apple.com/forums/thread/99931) -- `NSURLVolumeIsEncryptedKey` returns true for APFS whether or not the volume is encrypted
- [Decrypting an APFS encrypted volume using diskutil](https://derflounder.wordpress.com/2017/12/31/decrypting-an-apfs-encrypted-volume-using-diskutil-on-macos-10-13-2/) -- `diskutil apfs list` as the practical encryption query on APFS

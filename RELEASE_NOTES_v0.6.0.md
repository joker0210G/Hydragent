# Release Notes — v0.6.0

**Hydragent v0.6.0 — "Locked Memory"**
Released: 2026-06-14

This is a security-hardening release. It does not add any user-facing
features; instead, it tightens the foundations that hold secrets in the
agent's memory. Four new modules inside `hydragent-vault` ensure that
keys, derived sub-keys, and rotation reports are:

- **Pinned in physical RAM** (when the OS permits) so they cannot leak
  to the swap file.
- **Zeroized on drop** so they cannot survive in freed heap pages.
- **Bound to the column they protect** so a ciphertext cannot be
  silently re-targeted at a different column.

> **Scope note**: this release is **Track 6.4 of Phase 6 only** (Weeks
> 23–26 of the roadmap). It is not "Phase 6 complete." The remaining
> tracks — Merkle audit chain, Ed25519 action signing, the unified
> `hydragent-security` pipeline crate — will land in subsequent 0.6.x
> releases. The version bump from 0.5.0 → 0.6.0 reflects the addition
> of a new public security surface (`Rotator`, `ColumnCipher`,
> `SecureBuffer`, `mlock`), not the completion of the entire phase.
>
> **Post-MVP deferral (2026-06-14):** Track 6.5 (SQLCipher at-rest
> encryption for `data/memory/`, `data/audit/`, `data/sessions/`) is out
> of scope for the MVP. Column-AES inside the vault already covers the
> secrets; SQLite database files remain plaintext on disk until a
> post-MVP hardening pass.

---

## What's new in 30 seconds

- Every byte of a credential that lives in RAM is now wrapped in a
  **`SecureBuffer<T>`** — a heap allocation that calls `mlock` on
  construction and `zeroize` on drop.
- A new **`ColumnCipher`** provides per-column AES-256-GCM encryption.
  Each column is encrypted with a fresh sub-key derived from the master
  key via HKDF-SHA256, and the column name is bound as AAD.
- A new **`Rotator`** supports zero-downtime credential rotation.
  `rotate_passphrase` re-encrypts every entry with a new master key
  and atomically swaps the vault file. `rotate_column_key` generates a
  new 32-byte column key (the master key from which all column sub-keys
  are derived).
- A cross-platform **`mlock`/`munlock`** wrapper handles Unix
  (`libc::mlock`) and Windows (`VirtualLock`) transparently.
  `is_mlock_available()` reports runtime support.

---

## For users

### mlock-pinned secret buffers

If you are storing sensitive material in the agent's memory and want it
to be paged out of swap, wrap it in a `SecureBuffer`:

```rust
use hydragent_vault::secure_buffer::SecureBuffer;

let key: SecureBuffer<[u8; 32]> = SecureBuffer::new([0u8; 32])?;
// The 32 bytes are now mlock-pinned in physical RAM.
// On Drop, the buffer is zeroed and the pages are released.
```

> **Platform note**: on Linux, `mlock` may require `CAP_IPC_LOCK` or
> root to succeed. On Windows, `VirtualLock` succeeds for any process
> but pages are still pageable at the OS's discretion. In both cases,
> the zeroize-on-drop guarantee still holds regardless of mlock
> success.

### Column-level encryption

To encrypt individual database columns with different sub-keys:

```rust
use hydragent_vault::column_cipher::ColumnCipher;

let master: [u8; 32] = [/* your master column key */];
let cipher = ColumnCipher::new(&master);

let email_blob = cipher.encrypt("email", b"alice@example.com")?;
let ssn_blob   = cipher.encrypt("ssn",   b"123-45-6789")?;

// Decryption fails if the column name is wrong:
cipher.decrypt("credit_card", &ssn_blob)?; // Err(...)
```

The sub-key for each column is derived via HKDF-SHA256, so compromising
one column's sub-key does not compromise any other column, and rotating
the master column key invalidates *all* sub-keys in one shot.

### Credential rotation

```rust
use hydragent_vault::rotator::Rotator;

let rotator = Rotator::new(vault_path);

// Rotate the passphrase:
let report = rotator.rotate_passphrase("old-pp", "new-pp")?;
assert_eq!(report.entries_after, /* number of entries */);

// Rotate the column key (the 32-byte key all column sub-keys
// are derived from):
let (report, new_key) = rotator.rotate_column_key("current-pp")?;
let new_key_hex = report.new_column_key_hex.expect("set");
```

Both operations write to a temp file and atomically rename it over the
live vault, so a crash mid-rotation cannot leave a partially-written
file.

---

## Test coverage

| Surface                                | Tests | Status |
|----------------------------------------|-------|--------|
| `hydragent-vault` (unit)               | 56    | ✅     |
| `hydragent-vault` (doctest)            | 1     | ✅     |
| `hydragent-vault` (integration: column_cipher) | 7     | ✅     |
| `hydragent-vault` (integration: rotation)      | 9     | ✅     |
| `hydragent-vault` (integration: secure_buffer) | 6     | ✅     |
| **Track 6.4 total**                    | **79** | ✅    |

All Phase 1–5 tests remain green. The full repo exceeds 380 tests.

---

## Migration from 0.5.x

No breaking changes. Track 6.4 is purely additive to `hydragent-vault`.
If you were calling `Vault::save`/`Vault::load` with a
`HashMap<String, TaintedString>`, the API and on-disk format are
unchanged; the new types (`Rotator`, `ColumnCipher`, `SecureBuffer`,
`mlock`) are exposed as additional public API on the vault crate.

---

## Acknowledgements

This release draws on the prior art of:

- **IronClaw** (NEAR AI) — boundary key injection, adversarial
  evaluation.
- **OpenFang** — 16-layer cryptographic security pipeline.
- **libsodium** — `mlock` semantics and zeroize-on-drop pattern.

The column-AES-with-AAD pattern is borrowed from the design of
authenticated database encryption schemes (e.g. CipherSweet, Google's
"envelope encryption" for BigQuery).

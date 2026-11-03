# Security audit, October 2026

A review of the whole codebase as it stood on October 23, 2026, followed by fixes. The review
covered cryptography, storage and crash safety, every `unsafe` block, the app's handling of
secrets, dependencies and the Windows integration.

**Result: no critical or high-severity issues.** Six medium and nine low findings, listed below
with what was done about each.

## Medium

| # | Finding | Status |
|---|---|---|
| M1 | Records could be rolled back, replayed or mixed: a revision copied over the live note, or the metadata of one save combined with the body of another, decrypted fine. | **Fixed.** Each save has a random id bound into the associated data of both halves; revisions are sealed under their own labels. Older vaults are upgraded on first unlock. Rolling back the *entire* vault and deleting single rows remain undetectable and are documented. |
| M2 | Changing the password kept the vault key, so an old header copy plus the old password still opened new notes. | **Fixed.** Password change rotates the vault key, re-encrypts everything in one transaction, replaces the recovery key and deletes old backups. |
| M3 | One undecryptable note made the whole note list fail. | **Fixed.** Unreadable notes are reported and skipped, never deleted. |
| M4 | A failed save cleared the "unsaved" state, and a crashed storage thread dropped later saves silently. | **Fixed.** Saves are confirmed by the storage thread before "Saved" shows; failures are retried; a crashed worker shows a blocking error. |
| M5 | Copied note text and the recovery key stayed on the system clipboard after locking or quitting. | **Fixed.** Inkveil clears the clipboard on lock, quit and after setup if it still holds what Inkveil put there; a copied recovery key is cleared after 60 seconds. |
| M6 | Backups, the integrity check and Windows Hello were documented but not implemented. | **Fixed / corrected.** Rotating daily backups (7 recent + 4 weekly) and an integrity check run at unlock. Windows Hello is now documented as not yet implemented. |

## Low

| # | Finding | Status |
|---|---|---|
| L1 | Trash purge trusted the system clock; a wrong clock could delete trashed notes. | **Fixed.** Nothing is purged automatically; the app asks first. |
| L2 | The header copy was not used on database errors, and was read without a size limit. | **Fixed.** |
| L3 | A crafted header could make unlocking take hours. | **Fixed.** At most 16 slots and one password slot; Argon2 limited to 1 GiB and 16 passes. |
| L4 | The process protection comment claimed more than it delivers (threads, hooks, COM). | **Fixed and extended.** Comment corrected; DLL search restricted to System32, global-hook/AppInit DLLs blocked, image-load policy, linker `/DEPENDENTLOADFLAG`, Control Flow Guard. |
| L5 | A failed private clipboard write could leave unmarked text on the clipboard. | **Fixed.** Markers are written first and the clipboard is emptied on any error. |
| L6 | Decrypted text and key material could linger in memory. | **Improved.** Save buffers, Argon2 memory and the rich clipboard are wiped. Open notes still live in ordinary memory while unlocked (documented). |
| L7 | Auto-lock: only keystrokes counted, and no lock on Windows lock or sleep. | **Fixed.** Mouse and scroll count; configurable timeout; lock on Windows lock, sleep and hibernate. |
| L8 | A failure creating the recovery key during setup stranded the user. | **Fixed.** Setup continues with a warning; a new key can be created from settings. |
| L9 | Windows Error Reporting dumps could contain note text. | **Fixed.** Crash dumps exclude the heap. |

## Info items addressed

- `INKVEIL_VAULT_DIR` is honoured in debug builds only; there is no fallback to the current folder.
- GPUI is pinned to an exact commit instead of a tag.
- History thinning keeps the newest 20 revisions, one per day for 30 days, and the first one.

## Checked and found correct

XChaCha20-Poly1305 usage and nonce handling; HKDF key separation; key-slot binding to vault and
slot; Argon2 parameter validation; constant-time key comparison and redacted debug output;
parameterized SQL everywhere; WAL with `synchronous=FULL`, `secure_delete` and in-memory temp
storage; the crash-safe vault creation and header write order; every `unsafe` block in the
Windows integration; the masked password field; the link allow-list (`http`, `https`, `mailto`);
no networking code in the dependency tree.

## Still open

- Detecting deletion of single notes and whole-vault rollback.
- Windows Hello unlock.
- Code signing for release builds (see [RELEASING.md](RELEASING.md)).

# Follow-up review, November 2026

A review of a batch of changes (Humanize, smoother drag selection, multi-monitor window sizing,
a ten-note sidebar preview) and of how updates protect the vault. Everything below was fixed
before the changes were committed:

| # | Finding | Status |
|---|---|---|
| N1 | `build.rs` had been reduced to an icon: the `/DEPENDENTLOADFLAG:0x800` DLL-planting protection (L4) and the version information were gone, and resource errors were ignored. | **Reverted** to the hardened build script. |
| N2 | Text sent to the AI could carry invisible tag characters or variation selectors that models read as instructions ("ASCII smuggling"). | **Fixed.** Removed from requests unless part of an emoji flag or sequence. |
| N3 | The new reply cleanup rewrote the user's own text before sending it and stripped emoji joiners and selectors (breaking 👩‍💻, ❤️ and flags). | **Fixed.** Requests keep the user's text; replies lose hidden characters except inside emoji; Improve keeps typography the writer used. |
| N4 | The window-procedure hook resized windows in the middle of a drag between monitors (window jumping away from the pointer, possible back-and-forth between DPIs) and could send a 0×0 size when a DPI change arrived while minimized. | **Fixed.** The size is adapted once a drag ends; minimized windows are left alone; the maximized client-size correction (a real GPUI issue) is kept. |
| N5 | If `vault.db` went missing, the app offered setup; a new vault would overwrite `vault.header` and its backup rotation or a password change would delete the old vault's backups. | **Fixed.** Setup is refused while a header or backups remain, with recovery instructions. |
| N6 | Updates had no safety copy of the vault. | **Fixed.** `install.ps1` copies and verifies the vault first; documented in README, AGENTS.md and RELEASING.md. |

Since M5, a copied recovery key stays on the clipboard for two minutes instead of 60 seconds,
so there is time to paste it somewhere safe. [SECURITY.md](../SECURITY.md) describes the current
behaviour.

Still open, unchanged: Windows Hello unlock, hiding row counts in the database file, detecting
deletion of single notes and whole-vault rollback, code signing.

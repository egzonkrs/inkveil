# Architecture

Inkveil is a Cargo workspace. Each crate has one job, and only the platform crate may contain
`unsafe` code; every other crate has `#![forbid(unsafe_code)]`.

```
inkveil-app ──────────► inkveil-doc        rich-text model, edits, undo/redo, find, carets
    │                   inkveil-spell      offline spellcheck (bundled en-US dictionary)
    │                   inkveil-platform   capture exclusion, process hardening, private clipboard
    ▼
inkveil-store ────────► inkveil-crypto     keys, Argon2id, XChaCha20-Poly1305
```

Inside the app, `storage.rs` runs every vault operation on one worker thread; `search.rs` scans
note text for the sidebar search on that thread; `images.rs` cleans up and decodes pictures;
`ai.rs` and `secrets.rs` are the optional "Improve" and "Humanize" AI requests (with the cleanup
of hidden characters and typography in replies) and the check for secrets that runs before them.
A request runs on its own thread and streams the reply (server-sent events), reporting progress
to the panel over a channel; closing the panel drops the channel, which stops the request. A
read timeout catches a stalled stream and temporary failures are retried (`ai::MAX_RETRIES`).

## Keys

```
password ──Argon2id──► password key ─┐
recovery key ──HKDF──► recovery key ─┴─ each wraps ─► vault key (random, 256-bit)
                                                             ├─HKDF─► record key
                                                             └─HKDF─► picture fingerprint key
```

A platform slot (for Windows Hello) is reserved in the header format but not used yet.

The header listing the key slots is stored in the database and copied to `vault.header`.

## Storage

> [!WARNING]
> On any machine that uses Inkveil, `%LOCALAPPDATA%\Inkveil\vault` holds real notes. Never delete,
> rewrite or open it with other tools, and never test against it: use `.\start-dev.ps1 -TestVault`. A change to
> anything stored (tables, record layout, associated data, padding, header, key derivation)
> needs an upgrade for existing vaults and a test that opens a vault written by the previous
> version. See [AGENTS.md](../AGENTS.md).

Backups are `VACUUM INTO` copies in `backups\`, made at unlock and checked hourly while
unlocked (at most one every 20 hours, or on "Back up now"). If the user chose a backup folder,
the storage worker hands the newest one to a separate thread that copies it there
(`maintenance::copy_backups_to`), so a slow drive never blocks saves.

The vault folder holds `vault.db` (with its `-wal` and `-shm` files while open), `vault.header`,
`vault.lock`, `unlock-attempts` and `backups\`. A folder that has a header or backups but no
`vault.db` is treated as a vault whose database went missing: the app shows how to restore it
and `Vault::create` refuses to start a new vault there.

One SQLite database per vault, in WAL mode with `synchronous = FULL`.

| Table | Contents |
|---|---|
| `meta` | The key header |
| `notes` | One row per note: sealed metadata and sealed body |
| `revisions` | Snapshots of notes, for history |
| `state` | Sealed app state: open tabs, personal dictionary, settings, AI key |
| `blobs` | Pictures: random id, keyed fingerprint (to store each picture once), sealed bytes |

Pictures are not inside note bodies; an image block in a note holds the blob id. Blobs no note
or revision refers to are deleted after a day, during maintenance.

Metadata (title, tags, timestamps) and the body are sealed separately so the sidebar can list
every note without decrypting any bodies. Each sealed record's associated data names its vault,
table and row.

## Crash safety

- Each save is one SQLite transaction: after a crash the note is either the old or the new
  version.
- The app saves dirty notes about once a second while you type, so a power cut loses at most
  about a second of typing and an app crash loses less.
- New vaults are built under a temporary name and renamed into place once complete.
- Header updates go to the database first, then to the copy, with an atomic file replace.

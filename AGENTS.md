# Working on Inkveil

Rules for anyone changing this repository, people and AI coding agents alike.

## Never lose a real vault

People who work on Inkveil usually use it too, so the machine you are on probably has a real
vault: someone's only copy of their notes, pictures, settings and OpenRouter API key, encrypted
under their password. It lives here:

```
%LOCALAPPDATA%\Inkveil\vault\        ← the real vault
    vault.db, vault.db-wal, vault.db-shm   the encrypted database (the -wal file holds recent saves)
    vault.header                           copy of the key header (the wrapped vault key)
    vault.lock, unlock-attempts            instance lock, wrong-password counter
    backups\vault-<time>.db                the app's own daily backups
%LOCALAPPDATA%\Inkveil\update-backups\  ← copies made by scripts\install.ps1 before each update
<folder chosen in Settings>\Inkveil backups\  ← daily copies of backups\, if one was chosen
%LOCALAPPDATA%\Inkveil\*-archived-*\    ← older vaults; keep them
```

- **Never delete, move, rename, overwrite, "clean up", reset or recreate anything under
  `%LOCALAPPDATA%\Inkveil`.** Do not open these files with SQLite tools either: a write, a
  `VACUUM` or a checkpoint from outside the app can damage the vault, and the `-wal` file must
  stay with `vault.db`.
- **Develop and test with `.\start-dev.ps1 -TestVault`** (a throwaway vault in
  `target\dev-vault`) or with `INKVEIL_VAULT_DIR` set to a temporary folder. Plain
  `.\start-dev.ps1` opens the real vault. Release builds ignore `INKVEIL_VAULT_DIR` and always
  use the real vault.
- **Update the installed app only with `.\scripts\install.ps1`.** It closes Inkveil politely
  (which saves every note), copies the vault to `update-backups` and checks the copy, and only
  then replaces the program. If the copy fails, nothing is installed.
- **If Inkveil shows the setup screen ("create a vault") on a machine that has notes, stop.** Do not
  create a vault: the database has gone missing. The app now refuses to create a vault over a
  folder that still has a key header or backups, but do not test that on the real folder.
- **Storage format changes are dangerous.** Anything in `crates/inkveil-store` that changes what
  is written (tables, record layout, associated data labels, padding, the header, key
  derivation, `RECORDS_FORMAT`) can make the existing vault unreadable. Such a change needs an
  upgrade that runs on old vaults, a test that opens a vault written by the previous version,
  and testing on a copy of a dev vault. Never "fix" a vault by hand.
- **Backups.** Rotation (`maintenance::backups_to_delete`) decides which restore points survive;
  changing it can silently delete someone's backups. A password change screen, if added, must
  delete old-key copies everywhere: `backups\`, the chosen backup folder and `update-backups`.
  Copying to the backup folder must stay off the storage thread, so a hung drive never blocks
  saving.
- **Keys.** The password and the recovery key exist only with the user; the app never stores
  them and there is no back door. The vault key is stored only wrapped, in `vault.header` and in
  the database. Losing both copies of the header, or the database, loses everything.
- **Never read, print, log or send note contents, the API key, passwords or recovery keys,**
  even while debugging.

## Security rules

- Keep the hardening: `crates/inkveil-app/build.rs` (`/DEPENDENTLOADFLAG:0x800` against DLL
  planting, version information), `.cargo/config.toml` (Control Flow Guard), `harden_process`,
  capture exclusion, clipboard markers, and `#![forbid(unsafe_code)]` in every crate except
  `inkveil-platform`.
- The only network code is `crates/inkveil-app/src/ai.rs`: HTTPS to OpenRouter, only the selected
  text, only when the user clicks Improve or Humanize, after the secret scan.
- Every `unsafe` block states why it is sound.
- When security behaviour changes, update `SECURITY.md` and `docs/SECURITY-AUDIT.md` to match.
  Write down only what the code does; do not claim protections that are not there.

## Before handing over

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

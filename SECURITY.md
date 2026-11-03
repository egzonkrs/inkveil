# Security

## Reporting a vulnerability

Please report security problems privately through GitHub's "Report a vulnerability" button on
this repository rather than in a public issue.

## Threat model

### Inkveil protects against

- **Someone who gets the vault files**: a stolen or lost laptop, a disk pulled from a machine,
  another user account on the same computer, a backup or sync tool that copies the folder. They
  see random-looking data. They learn roughly how many notes and revisions exist and their
  approximate sizes (padded to 256-byte blocks), and nothing else: titles, tags, text, timestamps
  and settings are all encrypted.
- **Tampering with individual records.** Every record is authenticated and bound to its vault,
  table, note and save. Altered records, records moved to another note, an old revision copied
  over the live note, or the metadata of one save combined with the body of another are rejected.
  A rejected note is reported and left untouched; it does not hide the others.
- **Password guessing against stolen files.** Passwords go through Argon2id (256 MiB, tuned to
  about one second on the machine that created the vault), which makes each guess expensive.
- **Password guessing at the unlock screen.** After 15 wrong passwords or recovery keys in a row,
  the vault refuses attempts for a growing wait (30 seconds, then 1, 2, 5, 10 and at most 15
  minutes). The count is kept next to the vault, so restarting the app does not reset it; a
  correct unlock does. This only slows guessing through the app: someone who copies the vault
  files can guess offline, where Argon2id and the strength of your password are the protection.
- **A leaked old password.** Changing the password replaces the vault key and re-encrypts every
  record, so an old copy of the vault plus the old password cannot read anything written after the
  change. Old backups are deleted at that moment.
- **Screen capture and recording on Windows.** The window is excluded from capture
  (`WDA_EXCLUDEFROMCAPTURE`): OBS, Teams/Zoom sharing, the Snipping Tool and Windows Recall see
  nothing.
- **Clipboard leaks.** Copied note text is marked so Windows clipboard history and cloud
  clipboard do not keep it, and it is cleared from the clipboard when the vault locks or the app
  quits (unless something else was copied since).
- **Leaving the computer unlocked.** Auto-lock after 1, 5, 15 or 30 minutes, 1 or 4 hours of no
  input (or never), and optionally whenever Windows locks or the PC sleeps or hibernates.
- **Simple code injection by other programs running as you.** Other non-admin programs cannot open
  the process to read or write its memory or start threads in it; DLLs load from System32 only;
  global hook DLLs and AppInit DLLs are blocked; the binary uses ASLR, DEP and Control Flow Guard.
- **Pictures leaking.** Pictures are re-encoded when added (so location, camera and other
  metadata are dropped), then stored encrypted in the vault like notes, and decrypted only in
  memory while on screen; decoded pictures are dropped when the vault locks. Decoding runs with
  size limits. Copying a picture marks it like copied text. Saving a picture as a file writes an
  unencrypted copy where you choose; the menu says so. Pictures are never sent to the AI.
- **Notes leaking through the AI features.** "Improve" and "Humanize" are off until you add an
  OpenRouter API key. They send only the text you selected, only when you click them, after a
  check for keys, passwords and other secrets in it, over HTTPS to `openrouter.ai` (no redirects,
  no cookies, bundled root certificates), and ask OpenRouter to route only to providers that do
  not collect prompts. Titles, tags and the rest of the note are never sent. The key is stored
  encrypted in the vault and cleared from memory when it locks. Text you do send is handled under
  the policies of OpenRouter and of the provider that runs the model you chose, not Inkveil's. Replies are streamed; any reasoning the
  provider streams back only shows that the model is busy and is discarded, never stored or shown.
- **Hidden text in AI requests and replies.** Invisible tag characters and supplementary
  variation selectors, which models can read but you cannot see (a way to smuggle instructions
  into pasted text), are removed from what is sent unless they belong to an emoji. Invisible
  characters in the reply (zero-width spaces and joiners, direction marks, tags) are removed
  before you see the suggestion, again except inside emoji. Nothing changes your note until you
  accept the highlighted suggestion.
- **Losing notes to crashes or power cuts.** Every save is one durable SQLite transaction and is
  confirmed before the app shows "Saved"; failed saves are retried; history, daily backups and two
  copies of the key header are kept. Nothing is deleted without confirmation.
- **Losing notes to a failed disk or a lost PC,** if you choose a backup folder elsewhere
  (another drive, a USB stick, a synced folder). Each day's backup is copied there on a separate
  thread, so a slow or missing drive never holds up saving, and checked byte for byte before it
  gets its final name. The copies are the vault's own encrypted backups: they show the same as
  the vault folder does (roughly how many notes and revisions there are and their sizes), open
  only with your password or recovery key, and can be attacked offline by someone who gets them,
  where Argon2id and the strength of your password protect them. A synced folder means the sync
  provider holds such copies.
- **Losing notes to an update or a missing database.** `scripts\install.ps1` copies the whole vault
  to `%LOCALAPPDATA%\Inkveil\update-backups` and checks the copy before replacing the program. If
  `vault.db` disappears but its key header or backups are still there, Inkveil refuses to create a
  new vault over them (which would replace the header and, through backup rotation, the old
  backups) and explains how to put the database back.

### Inkveil cannot protect against

- **Malware running as administrator, or as you with enough effort,** while the vault is
  unlocked. It can log keystrokes, read the screen by other means, or attack the process through
  paths that do not need a process handle. Inkveil makes this harder but no app can stop it.
- **Rolling back the whole vault.** Someone who can replace the entire vault folder with an older
  copy of it (for example from a backup) gets an older but valid vault; this cannot be detected
  without a trusted record of the latest state. Deleting single notes from the database is also
  not detected yet.
- **Decrypted text in memory and in the page or hibernation file.** Keys, passwords and save
  buffers are wiped after use, but the text of open notes lives in ordinary memory while the vault
  is unlocked, and Windows may write it to the page file or hiberfil.sys. Full-disk encryption
  (BitLocker) protects those files.
- **Someone at your unlocked computer** before auto-lock triggers.
- **A forgotten password with no recovery key.** There is no back door. Keep your recovery key
  somewhere safe.
- **Old copies after a password change.** Changing the password deletes the vault's own backups,
  but not the pre-update copies in `%LOCALAPPDATA%\Inkveil\update-backups` or the copies in your
  backup folder, which still open with the old password and show the notes as they were then.
  (The app has no password change screen yet; whoever adds one must delete those too.) If you change your password because the
  old one may have leaked, delete that folder afterwards.
- **Statistical AI watermarks.** "Humanize" removes hidden characters, but a provider that marks
  text through its choice of words cannot be undone by the app; the rewrite is itself model
  output.

### Platform notes

- **Windows**: complete capture exclusion on Windows 10 version 2004 and later.
- **macOS** (planned): capture exclusion via `NSWindow.sharingType = .none` is not honored by
  every capture path on recent macOS versions. Touch ID will require a signed build.
- **Linux** (planned): no desktop API lets a window hide itself from capture. Passwordless unlock
  can only use the login keyring, which other apps running as you can also query.
- **Passwordless unlock** (Windows Hello) is not implemented yet.

## Cryptography

| Purpose | Algorithm |
|---|---|
| Record encryption | XChaCha20-Poly1305, random 192-bit nonce per record |
| Associated data | Label, vault id, note id and a random per-save id |
| Password to key | Argon2id v1.3, 256 MiB, ≥3 passes, 4 lanes, 128-bit salt |
| Key separation, recovery keys | HKDF-SHA256 |
| Recovery key | 160 random bits, Crockford base32 |

A random 256-bit vault key encrypts all records. It is stored only wrapped (encrypted) under the
key of each unlock method. All primitives come from the [RustCrypto](https://github.com/RustCrypto)
project; nothing is home-made.

See [docs/SECURITY-AUDIT.md](docs/SECURITY-AUDIT.md) for the latest audit and the status of each
finding.

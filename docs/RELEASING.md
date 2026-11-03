# Releasing and code signing

## Why sign

An unsigned program that hides its window from capture and locks down its own process looks, to
an antivirus heuristic, a lot like spyware. Bitdefender quarantined an unsigned debug build during
development. A code-signing signature:

- proves who published the binary, and that it was not modified since (Windows shows a modified
  file as having an invalid signature, so a tampered copy cannot pass as genuine);
- lets antivirus products and Microsoft SmartScreen build reputation for the publisher instead of
  treating every build as a brand-new unknown file.

Signing needs a certificate tied to a verified identity, so it cannot be done for you
automatically. Options, roughly from cheapest:

| Option | Cost | Notes |
|---|---|---|
| [SignPath Foundation](https://signpath.org) | Free for open-source projects | Requires a public repository with an OSI license and builds on CI. Good fit once Inkveil is public. |
| [Certum Open Source Code Signing](https://shop.certum.eu) | Low yearly fee | For individual open-source developers worldwide; identity check; key in a cloud HSM or smart card. |
| Azure Trusted Signing (Microsoft) | Monthly fee | Fully managed keys and CI integration; check current eligibility for individuals in your country. |
| Commercial OV certificate | Higher yearly fee | Must live on a hardware token or HSM. |
| Microsoft Store (MSIX) | Low or no cost | The Store signs the package; updates go through the Store. |

Reputation still builds over time even with a signature; the first releases may see SmartScreen
prompts.

## Building a release

```powershell
.\scripts\release.ps1                         # unsigned build into dist\
.\scripts\release.ps1 -CertThumbprint <sha1>  # signed with a certificate in your Windows store
```

The script builds with `--release`, signs `inkveil.exe` with `signtool` (SHA-256 file digest and an
RFC 3161 timestamp, so the signature stays valid after the certificate expires) when a certificate
is given, verifies the signature, and writes the SHA-256 hash next to the binary.

Before publishing, also submit the signed binary to antivirus vendors that flag it (for example
Bitdefender's false-positive form) and to Microsoft's
[malware analysis portal](https://www.microsoft.com/wdsi/filesubmission).

## Updating an installed copy without losing notes

`.\scripts\install.ps1` is the only supported way to update the app on a machine with real notes.
It builds, closes Inkveil through its window (which saves every note and waits for the storage
thread), copies `%LOCALAPPDATA%\Inkveil\vault` to `%LOCALAPPDATA%\Inkveil\update-backups` and
compares every file by hash, and only then copies the new `Inkveil.exe` into place. It never
writes to the vault folder.

Before releasing a build that changes anything in `crates/inkveil-store`, open a vault written by
the previous release with it (a copy of a dev vault, never the real one) and check that notes,
pictures, history, settings and the AI key all come through.

# Security policy

Phase LAN connects players' computers over the internet, so we take security reports seriously.

## Supported versions

Only the latest release gets security fixes. Please update by installing the newest release from the [website](https://phase.yeskendir.com/lan/download/) or [GitHub Releases](https://github.com/phase-mc/phase-lan/releases).

## Reporting a vulnerability

**Please do not open a public issue.** Report it privately through GitHub: open the repository's **Security** tab and choose **Report a vulnerability** ([direct link](https://github.com/phase-mc/phase-lan/security/advisories/new)).

Please include:

- what an attacker can do and what they need (a room code, a position on the network, a malicious mod, a crafted file);
- the Phase version and Windows version;
- steps to reproduce, or a proof of concept.

We will confirm the report within 7 days, keep you updated, and credit you in the release notes unless you prefer otherwise. Please give us a reasonable time to ship a fix before disclosing it publicly.

## What is in scope

- Joining a room without the host's approval, or impersonating another device.
- Reading or changing game, voice or mod traffic between players.
- Mod sync writing files outside the `mods` folder, or installing a file whose hash does not match.
- Code execution through invites, room codes, `phase://` links, the relay or the room code service.
- Leaking the device key or other local data.
- The installer or uninstaller touching files outside its own folders.

Problems in Minecraft itself, in third-party mods or in services such as Modrinth should be reported to their authors.

## Verifying a download

Every release is built by [GitHub Actions](.github/workflows/release.yml) from a tagged commit. The SHA-256 of the installer is listed in the release notes and in `release.json`:

```powershell
Get-FileHash .\Phase-Setup-*.exe -Algorithm SHA256
```

If the hash does not match, do not run the file and let us know where you downloaded it.

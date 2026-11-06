# Security Policy

Thank you for reporting security vulnerabilities responsibly before any public
disclosure.

## Supported Versions

WaveFlow does not have long-term version support yet. Security fixes land on
`main` and ship in the next release of each update channel; an installation is
expected to follow its channel's updates.

| Version                                           | Supported |
| ------------------------------------------------- | --------- |
| `main`                                            | Yes       |
| Latest stable release                             | Yes       |
| Latest beta, on the in-app beta channel           | Yes       |
| Older versions, snapshots, and unmaintained forks | No        |

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.**

Use one of these channels, depending on what is available on the public
repository:

1. **GitHub Security Advisories**: open the repository's _Security_ tab, then
   choose _Report a vulnerability_. This is the recommended confidential
   channel.
2. If GitHub Security Advisories are not available, or GitHub itself is not,
   write to the maintainer at the address given in
   [CODE_OF_CONDUCT.md](../CODE_OF_CONDUCT.md#reporting-an-issue).

Your report should include:

- a clear description of the issue;
- the expected impact on users or their data;
- the affected version, platform, and installation mode;
- the affected files, Tauri commands, integrations, or user flows;
- detailed reproduction steps;
- a minimal proof of concept if needed to understand the issue;
- a suggested mitigation if you have one;
- your contact information for follow-up.

## Response Targets

- Initial acknowledgement: within 3 business days.
- First assessment: within 7 business days.
- Follow-up updates: at least once per week until the fix is released.
- Public disclosure: coordinated after the fix is released, usually within 30
  to 90 days depending on severity and impact.

## Current Scope

The most sensitive WaveFlow surfaces are:

- access to local files and audio library folders, and what WaveFlow writes
  there (tags, lyrics, covers);
- parsing of untrusted files: audio containers and their tags (ID3v2 chapters
  included), artwork, lyrics (LRC, TTML, VTT, SRT), M3U/M3U8, CSV and JSON
  playlist imports, OPML;
- WebDAV and SMB shares read as library folders, and their stored credentials;
- podcast feeds, enclosures, chapters and transcripts, which are read as
  hostile input, and the download folder and its quota;
- the plugin host: WebAssembly plugins run in a wasmtime sandbox, the plugin
  store and its registry (blake3-pinned), plugin options and credentials;
- Tauri commands exposed to the frontend, and the webview's Content Security
  Policy;
- local SQLite databases, profiles, backups and profile export archives;
- the optional LAN servers (DLNA MediaServer, MPD) and the WaveFlow server
  connection (sync, streaming, uploads);
- online integrations: Deezer, Last.fm, ListenBrainz, MusicBrainz and the Cover
  Art Archive, TheAudioDB, LRCLIB and other lyrics providers, Discord Rich
  Presence, web radio;
- Last.fm and ListenBrainz tokens, and the scrobbling queue;
- the signed update mechanism, read from GitHub and from its mirror on
  `download.waveflow.app`.

## Out of Scope

The following reports are generally not considered exploitable security
vulnerabilities:

- cosmetic bugs or interface issues without security impact;
- generic automated reports without a demonstrated exploit path;
- vulnerabilities in unmodified third-party services;
- issues that already require full access to the user's account or local
  machine without increasing impact;
- voluntary disclosure of files or secrets by the user.

## Rewards

WaveFlow does not offer a monetary bug bounty. Researchers who report a valid
vulnerability may be credited in the fix release notes if they want attribution.

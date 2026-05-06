# Security Policy

Thank you for reporting security vulnerabilities responsibly before any public
disclosure.

## Supported Versions

WaveFlow does not have long-term version support yet. The main branch and the
latest published release should be considered the only supported versions for
security fixes.

| Version | Supported |
| --- | --- |
| `main` / latest published release | Yes |
| Older versions, snapshots, and unmaintained forks | No |

## Reporting a Vulnerability

**Do not open a public issue for security vulnerabilities.**

Use one of these channels, depending on what is available on the public
repository:

1. **GitHub Security Advisories**: open the repository's *Security* tab, then
   choose *Report a vulnerability*. This is the recommended confidential
   channel.
2. Contact the maintainers privately if GitHub Security Advisories are not
   available.

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

- access to local files and audio library folders;
- M3U/M3U8 playlist import and export;
- metadata and artwork extraction from audio files;
- Tauri commands exposed to the frontend;
- local SQLite databases, profiles, and metadata caches;
- Deezer, Last.fm, and LRCLIB integrations;
- Last.fm authentication, session tokens, and the scrobbling queue;
- the signed Tauri update mechanism.

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

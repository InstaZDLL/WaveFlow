# Privacy policy

_WaveFlow desktop, version 2.0 and later. Last updated: 7 November 2026._

WaveFlow is a music player for the files on your computer. It has **no account, no telemetry, no analytics and no crash reporting**: nothing about you or how you use it is sent to the WaveFlow project. Your library, your listening history, your playlists and your settings stay in databases on your computer.

Some features do reach the network. Each of them is listed below with what it sends, when, whether it is on when you first install WaveFlow, and how to turn it off. When a feature sends something, it goes **directly from your computer to the service named**, never through a server run by the WaveFlow project, except the update manifest and the plugin registry, which are files the project publishes.

## In short

| Feature                                         | On by default                 | What leaves your computer                                 |
| ----------------------------------------------- | ----------------------------- | --------------------------------------------------------- |
| Update check                                    | Yes                           | A request for the latest version number                   |
| Album covers and artist pictures (Deezer)       | Yes                           | Album titles and artist names                             |
| Artist backdrops and biographies (TheAudioDB)   | Yes                           | Artist names                                              |
| Lyrics search                                   | Yes                           | Track title, artist, album and length                     |
| Discord Rich Presence                           | Yes, when Discord is running  | What you are playing, to the Discord app on your computer |
| Plugin store                                    | When you open it              | A request for the list of plugins                         |
| Plugins you install, Web Radio                  | When you use them             | What each plugin needs (see below)                        |
| Last.fm, ListenBrainz                           | No, needs your account        | What you play                                             |
| Library enrichment, Release Radar (MusicBrainz) | No                            | Album titles and artist names                             |
| Podcasts                                        | No                            | Requests to the feeds you subscribe to                    |
| WaveFlow Server, WebDAV and SMB shares          | No, needs an address you give | Whatever you sync, stream or read                         |
| DLNA and MPD servers                            | No                            | Your library, to devices on your local network            |

Every service you reach also sees your **IP address** and the time of the request, as with any connection on the internet.

## Offline mode

**Settings → Connections → Connectivity → Offline mode** stops WaveFlow from contacting the online services in this list: the update check, covers and pictures, biographies, lyrics, Last.fm and ListenBrainz, MusicBrainz, the plugin store and the plugins' own requests, radio, podcasts, WaveFlow Server and network shares. What was already fetched stays in its cache and is still shown.

Offline mode does **not** stop:

- Discord Rich Presence, which talks to the Discord app on your computer; turn it off on its own switch;
- the DLNA and MPD servers, which only serve your local network and are off unless you turn them on;
- possibly, a picture that was never fetched before and that the interface loads straight from its address, such as a photo in a cover or artist picker.

## Each feature in detail

### Update check

When it starts, and when you switch the beta channel on or off, WaveFlow asks for the latest version: it requests a small manifest from GitHub (`github.com`, under `InstaZDLL/WaveFlow/releases`) and from the project's mirror, `download.waveflow.app`, served by Cloudflare. The request carries no information about you beyond what any download does. The update itself is downloaded only when you choose to install it, and its signature is checked before it runs. The beta channel reads two more manifests at the same places.

Turn the automatic check off in **Settings → Maintenance → Updates → Check for updates automatically**; WaveFlow then asks only when you press **Check now** on the same card. Offline mode stops the automatic check too, and refuses a check or an install you ask for. Builds made without the `updater` feature, such as distribution packages that update through their package manager, do not check at all.

### Album covers and artist pictures — Deezer

To find a cover or an artist photo that your files do not carry, WaveFlow searches Deezer's public catalogue (`api.deezer.com`, images from `dzcdn.net`) with the album title and artist name, or the artist name alone. This happens when you open an album or artist page, when the artist playing changes, when a new track starts while Discord Rich Presence is on and the Discord app is running (the cover shown in Discord; once found, it is reused for the album's other tracks), when a radio station moves to a new song (that song's cover in the player, searched by its title and artist), when you search for a cover yourself, and when you import a public Deezer playlist link. Results are cached on your computer and reused. No Deezer account is involved. Offline mode turns it off.

### Artist backdrops and biographies — TheAudioDB

When an artist page is filled in, WaveFlow asks TheAudioDB (`theaudiodb.com`) for that artist's backdrop and, if you chose it as the biography source, the biography, sending the artist name and the language you read biographies in. Offline mode turns it off.

### Lyrics

When lyrics are shown for a track that has none in its file, next to it, or in WaveFlow's cache, WaveFlow searches the lyrics providers in turn with the track's title, artist, album and length: **LRCLIB** (`lrclib.net`) first, then **NetEase** (`music.163.com`) and **Megalobiz** (`megalobiz.com`). Genius is in the list but off by default. You can switch off any provider except LRCLIB, and skip the search entirely for the genres you choose (instrumental and lo-fi are skipped by default), in **Settings → Lyrics → Finding lyrics**. Offline mode turns all of them off. Lyrics found are kept on your computer.

### Discord Rich Presence

When the Discord app is running on your computer, WaveFlow tells it what you are playing: the title, artist and album, the cover's address, how far into the track you are, and whether it is paused. **Discord then shows this on your Discord profile**, to the people who can see your activity there. WaveFlow sends it to the Discord app over a local connection, not over the internet; Discord's own servers take it from there. Turn it off in **Settings → Connections → Connected services → Discord Rich Presence**.

### Plugin store and plugins

Opening the **Extensions** page reads the list of available plugins from `waveflow.app`, or, if that does not answer, from GitHub (`raw.githubusercontent.com`) or jsDelivr (`cdn.jsdelivr.net`). Installing a plugin downloads it from the address in that list and checks it against the fingerprint the list pins.

A plugin runs in a sandbox and can only reach the web addresses it declares; WaveFlow shows them, with the plugin's other permissions, before you install it. What a plugin sends depends on what it does: a lyrics plugin sends the track's title, artist, album and length to its source, an artwork or Canvas plugin the album or track it looks up. A plugin that asks for it may read the list of artists in your library, never your files, paths or listening history. Some plugins need an account cookie or token, which you enter in their options and which is sent only to their own service. Disable or uninstall a plugin from the Extensions page; offline mode blocks every plugin request.

**Web Radio** is a plugin that ships with WaveFlow. Browsing stations sends your searches, the country and the tags you pick to Radio Browser (`radio-browser.info`); playing a station connects to that station's own server.

### Last.fm and ListenBrainz

Off until you connect your account in **Settings → Connections**. Once connected, WaveFlow sends what you play — artist, title, album, track number, length and the time you played it — to **Last.fm** (`last.fm`) or **ListenBrainz** (`listenbrainz.org`) to record it on your profile, and Last.fm also provides artist biographies and similar artists. Logging in to Last.fm sends your username and password to Last.fm once; the password is not kept. Tracks you finish are queued on your computer and sent when a connection is available. Disconnecting stops it and clears the queue.

### Library enrichment and Release Radar — MusicBrainz

**Library enrichment** is off by default. Turned on in **Settings → Library → Enrichment**, it looks up your albums, tracks and artists on MusicBrainz (`musicbrainz.org`) and their covers on the Cover Art Archive (`coverartarchive.org`, served by the Internet Archive), sending titles and artist names. What it finds is written into WaveFlow's database only, never into your files. **Release Radar** and the **tag workshop** send the same kind of request when you start them yourself.

### Podcasts

Off by default, and turned on only after a notice that explains this: an episode comes straight from the server that hosts it, which sees that you are fetching it, as any podcast app's host does. Once on, WaveFlow fetches the feeds you subscribe to every six hours while it runs, and the episodes, chapters and transcripts you play or download.

### WaveFlow Server and network shares

Only when you link a [WaveFlow Server](https://github.com/InstaZDLL/waveflow-server) or add a WebDAV or SMB share, at an address you give. WaveFlow then talks to that server or share alone, with the credentials you entered: it reads, streams and, for WaveFlow Server, syncs your favourites, ratings, playlists and plays, and uploads the tracks you choose to send.

### DLNA and MPD servers

Both off by default, in **Settings → Connections → Network sharing**. Turned on, they make WaveFlow visible on your **local network**: the DLNA server lets other devices browse and play your library, including the audio files themselves, without a password; the MPD server lets an MPD client see your queue and control playback, with an optional password. Nothing is sent to the internet.

## What stays on your computer

- **Your library and your history**: one database per profile, holding your tracks, playlists, likes, ratings, play history and settings, and one shared database for app-wide settings and caches.
- **Caches**: covers, artist pictures, lyrics, Canvas clips and downloaded episodes, in a folder you can move in **Settings → Storage and backups**.
- **Logs**: kept for seven days on your computer. **Settings → Maintenance → Diagnostics → Copy logs** copies them to your clipboard so you can paste them into a report yourself; nothing is uploaded.
- **Credentials**: your Last.fm session, your ListenBrainz token, your WaveFlow Server tokens, the passwords of your network shares and your MPD password are stored in WaveFlow's databases on your computer. They are not encrypted there, so anyone with access to your user account can read them.

Uninstalling WaveFlow may leave its data folders behind; remove them to erase everything.

## Third-party services

Each service WaveFlow can reach has its own privacy policy:

- Deezer — <https://www.deezer.com/legal/personal-datas>
- Last.fm — <https://www.last.fm/legal/privacy>
- MusicBrainz, ListenBrainz and the Cover Art Archive (MetaBrainz Foundation) — <https://metabrainz.org/privacy>
- Discord — <https://discord.com/privacy>
- GitHub — <https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement>
- Cloudflare, which serves `download.waveflow.app` and `waveflow.app` — <https://www.cloudflare.com/privacypolicy/>
- jsDelivr — <https://www.jsdelivr.com/terms/privacy-policy-jsdelivr-net>
- Genius — <https://genius.com/static/privacy_policy>
- LRCLIB (<https://lrclib.net>), TheAudioDB (<https://www.theaudiodb.com>), Radio Browser (<https://www.radio-browser.info>), NetEase Cloud Music (<https://music.163.com>) and Megalobiz (<https://www.megalobiz.com>) — see their websites.

Plugins from the store, podcast feeds, radio stations and servers you add are run by others; their own policies apply.

## Changes and questions

This policy is updated with the app; its history is this file's history in the repository. For a question, [open a discussion](https://github.com/InstaZDLL/WaveFlow/discussions/categories/q-a); for anything private, use the confidential channel in [SECURITY.md](.github/SECURITY.md).

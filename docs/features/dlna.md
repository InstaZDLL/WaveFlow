# DLNA / UPnP MediaServer

WaveFlow exposes the active profile's library on the LAN as a `urn:schemas-upnp-org:device:MediaServer:1`. DLNA-compatible receivers (Yamaha MusicCast, Sonos S2, Kodi, BubbleUPnP, VLC, …) discover and stream the collection without any per-receiver pairing.

The integration ships disabled by default — enable it from **Settings → Connections → Network sharing → DLNA / UPnP server**.

## Architecture

A single dedicated worker thread (`dlna-worker`) owns a tokio runtime and the running tasks. Same pattern as [`media_controls`](../../src-tauri/crates/app/src/media_controls.rs) and [`discord_presence`](../../src-tauri/crates/app/src/discord_presence.rs): a sync `DlnaServer` handle on `AppState` ferries `Cmd::{Start, Stop, Status}` over a crossbeam channel so the rest of the app keeps a sync API.

```bash
AppState.dlna ─► Cmd channel ─► dlna-worker
                                 ├─► axum HTTP server (port N)
                                 │     /description.xml
                                 │     /service/{ContentDirectory,ConnectionManager}.xml
                                 │     /control/ContentDirectory  (SOAP)
                                 │     /control/ConnectionManager  (SOAP, stub)
                                 │     /stream/<track_id>          (Range)
                                 │     /art/<hash.ext>
                                 │     /healthz
                                 └─► SSDP announcer + responder (239.255.255.250:1900)
```

## Configuration

Persisted in the global `app_setting` table because the server is process-wide, not per-profile. Switching profiles re-binds the same listener to whatever the new profile points at.

| Key                | Default    | Note                                                                                                                              |
| ------------------ | ---------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `dlna.enabled`     | `0`        | Opt-in. Auto-started at boot when set.                                                                                            |
| `dlna.server_name` | `WaveFlow` | Friendly name shown in controllers.                                                                                               |
| `dlna.port`        | `0`        | `0` lets the OS pick a free port; the SSDP `LOCATION` carries the actual port. Pin a value if your firewall is configured for it. |

## Object hierarchy (ContentDirectory)

Object IDs are **string prefixes**, routed by [`cds.rs`](../../src-tauri/crates/app/src/dlna/cds.rs):

```bash
0                    Root container
├─ 0/artists         All artists (paginated)
│  └─ 0/artists/<id> Albums for that artist (containers)
└─ 0/albums          All albums (paginated)
   └─ 0/albums/<id>  Tracks for that album (items)

0/track/<id>         Single-track BrowseMetadata payload
```

Every id that a `BrowseDirectChildren` can return also answers `BrowseMetadata` with exactly one object: controllers read a folder's metadata before opening it, or before queueing it whole, and an empty answer shows as an empty folder. Every container carries a `childCount` (index-backed `COUNT` subqueries; an album counts only its available tracks, as its listing does). The `parentID` a `BrowseMetadata` reports:

| Object                  | `parentID`      | Why                                                                                                                     |
| ----------------------- | --------------- | ----------------------------------------------------------------------------------------------------------------------- |
| `0`                     | `-1`            | The root, per the spec.                                                                                                 |
| `0/artists`, `0/albums` | `0`             |                                                                                                                         |
| `0/artists/<id>`        | `0/artists`     |                                                                                                                         |
| `0/albums/<id>`         | `0/albums`      | Also listed under its artist, but its id lives under `0/albums`, which reaches it whatever the album's artist row says. |
| `0/track/<id>`          | `0/albums/<id>` | Its album, the only container that lists it; `0` for a track without an album.                                          |

An unknown id, or one whose row is gone, still answers an empty DIDL rather than a fault.

Pagination via `StartingIndex` + `RequestedCount` → SQL `LIMIT/OFFSET`, capped at `MAX_PAGE_SIZE = 500`. `RequestedCount = 0` (the spec's "all") folds to the same cap so a misbehaving controller can't pull 50k tracks into one DIDL document.

## DIDL-Lite items

Each track DIDL item carries:

- `dc:title`, `dc:creator`, `upnp:artist`, `upnp:album`
- `upnp:class = object.item.audioItem.musicTrack`
- `upnp:albumArtURI` pointing at `/art/<blake3>.<ext>` (probes both the per-profile `artwork/` dir and the shared `metadata_artwork/` dir; see [Artwork](#artwork) for what the route accepts)
- `<res protocolInfo="http-get:*:<mime>:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000…">` plus `duration` (`H:MM:SS.000`, padded for Sonos S2), `size`, `bitrate` (in DLNA bytes/s), `sampleFrequency`, `nrAudioChannels`.

The `transferMode.dlna.org: Streaming` and `contentFeatures.dlna.org` headers on `/stream/<id>` are mandatory for DLNA controllers to expose a scrubber.

## SSDP discovery

[`ssdp.rs`](../../src-tauri/crates/app/src/dlna/ssdp.rs) joins `239.255.255.250:1900` via socket2 (so we get `SO_REUSEADDR` on Windows + `SO_REUSEPORT` on unix and coexist with other UPnP services).

- **Periodic NOTIFY ssdp:alive** — one batch every `CACHE_MAX_AGE/4` ≈ 7 minutes, advertising `upnp:rootdevice`, the device UUID, `MediaServer:1`, `ContentDirectory:1`, `ConnectionManager:1`.
- **M-SEARCH responder** — unicast HTTP/1.1 200 OK to controllers that probe with `ST:` matching one of our targets (or `ssdp:all`).

The device UUID is `Uuid::new_v5(NAMESPACE, server_name)` so controllers see the same `uuid:` URN across launches even when the LAN IP changes — no on-disk persistence needed.

## Which address is advertised

A desktop often carries a VPN, WSL, Hyper-V or Docker adapter next to its real LAN, frequently listed first, and an address on one of those is one a receiver cannot reach. So nothing advertises a single address picked once:

- **SSDP** joins the multicast group on **every** IPv4 interface (one that refuses the membership is skipped; only refusing them all fails). Each NOTIFY batch goes out of each interface with that interface's own address in `LOCATION`, and an M-SEARCH reply names the address on the requester's subnet — the preferred address when none matches.
- **HTTP** — `URLBase` in `/description.xml`, and every stream and cover URL in a Browse result, is built from the local address the request arrived on (`http::LocalAddr`, recorded through axum's `ConnectInfo`). The address a controller connected to is reachable from it by construction, whichever interface that is.
- **The preferred address** — the URL shown in Settings, and the fallback above — comes from [`lan_addrs`](../../src-tauri/crates/app/src/dlna/mod.rs), which ranks every non-loopback IPv4 address: a private address on a running, physical interface first; then a non-private one; then anything point-to-point, down, or named like a VPN / VM / container adapter (`vEthernet (…)`, `docker0`, `br-…`, `virbr0`, `tailscale0`, `wg0`, …); link-local (`169.254.x.x`) last. The name match is a heuristic, so it only demotes, never drops: a host whose only address looks virtual is still served on it. Within a rank the OS order holds.

The interface list is read once when the server starts. After a network change (new Wi-Fi, a VPN coming up), toggle the server off and on to pick it up. There is no setting to pin an interface.

## Range streaming

`/stream/<track_id>` parses the `Range:` header, replies with `206 Partial Content` + `Content-Range`, and pipes the file through `tokio_util::io::ReaderStream::with_capacity(64 KiB)`. `take(length)` caps the reader so we never overshoot the window even if the controller closes early.

[`parse_range`](../../src-tauri/crates/app/src/dlna/http.rs) answers one of three ways, following RFC 9110:

| `Range:`                                                      | Answer                                                       |
| ------------------------------------------------------------- | ------------------------------------------------------------ |
| `bytes=S-E`, `bytes=S-`, `bytes=-N` naming a byte of the file | `206`; an `E` past the end is clamped to the last byte       |
| a first byte at or past the end, or `bytes=-0`                | `416 Range Not Satisfiable` + `Content-Range: bytes */<len>` |
| absent, malformed, another unit, several ranges               | `200` with the whole file — the header is ignored            |

An empty file always gets a plain `200` with `Content-Length: 0`. [`media_loopback`](../../src-tauri/crates/app/src/media_loopback.rs), which serves video to WebKitGTK, shares the same helper, so it answers the same way.

## Artwork

`/art/<name>` serves only a name of the shape `<hex hash>.<extension>` (`jpg`, `jpeg`, `png`, `webp`, `gif`, `bmp`, `tiff`, `bin`) — the shape Browse links to. Anything else is a `400` before the filesystem is touched.

`bin` is what the scanner names an embedded picture whose MIME type lofty could not map — most often a WebP tagged with an empty or non-standard type. Its `Content-Type` comes from its bytes ([`detect_image_format`](../../src-tauri/crates/core/src/artwork/image_format.rs)), and an unrecognised format is served as `application/octet-stream` rather than as a guessed JPEG.

A deny-list of `/`, `\` and `..` is not enough on Windows: a name with a drive prefix and no root, like `D:secret.txt`, _replaces_ the base in `PathBuf::join` instead of extending it, so the lookup would leave the artwork dir and read a file on that drive — from any peer on the LAN, with no authentication. An allow-list of the one shape the route has to serve leaves nothing to escape with.

## Error handling

Failures inside the worker are surfaced through `DlnaStatus.last_error` so the Settings UI can display them inline:

- Bind failure (port in use, no privilege) → `"bind 0.0.0.0:1234: …"` and the server stays in the stopped state.
- SSDP socket failure → HTTP keeps serving for controllers that already know the LOCATION; only auto-discovery breaks. Surfaced as `"SSDP: …"` — including `"SSDP: no IPv4 network interface"` when the host has no address to advertise, rather than announcing `0.0.0.0`.

## Limitations / not implemented yet

- No `Search` action — `GetSearchCapabilities` returns an empty string so controllers fall back to Browse.
- No event subscriptions (`SUBSCRIBE` / `NOTIFY` from the event sub URLs). Controllers that need them will retry; nothing breaks.
- DSD (`.dsf` / `.dff`) doesn't appear in the audio MIME mapping — would need its own `audio/x-dsd` advertisement once the playback path lands.
- The bind is `0.0.0.0`. If you're on a public Wi-Fi network, **disable the toggle** — there's no auth.

# DLNA / UPnP MediaServer

WaveFlow exposes the active profile's library on the LAN as a `urn:schemas-upnp-org:device:MediaServer:1`. DLNA-compatible receivers (Yamaha MusicCast, Sonos S2, Kodi, BubbleUPnP, VLC, …) discover and stream the collection without any per-receiver pairing.

The integration ships disabled by default — enable it from **Settings → Integrations → DLNA / UPnP Server**.

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

Pagination via `StartingIndex` + `RequestedCount` → SQL `LIMIT/OFFSET`, capped at `MAX_PAGE_SIZE = 500`. `RequestedCount = 0` (the spec's "all") folds to the same cap so a misbehaving controller can't pull 50k tracks into one DIDL document.

## DIDL-Lite items

Each track DIDL item carries:

- `dc:title`, `dc:creator`, `upnp:artist`, `upnp:album`
- `upnp:class = object.item.audioItem.musicTrack`
- `upnp:albumArtURI` pointing at `/art/<blake3>.<ext>` (probes both the per-profile `artwork/` dir and the shared `metadata_artwork/` dir)
- `<res protocolInfo="http-get:*:<mime>:DLNA.ORG_OP=01;DLNA.ORG_FLAGS=01700000…">` plus `duration` (`H:MM:SS.000`, padded for Sonos S2), `size`, `bitrate` (in DLNA bytes/s), `sampleFrequency`, `nrAudioChannels`.

The `transferMode.dlna.org: Streaming` and `contentFeatures.dlna.org` headers on `/stream/<id>` are mandatory for DLNA controllers to expose a scrubber.

## SSDP discovery

[`ssdp.rs`](../../src-tauri/crates/app/src/dlna/ssdp.rs) joins `239.255.255.250:1900` via socket2 (so we get `SO_REUSEADDR` on Windows + `SO_REUSEPORT` on unix and coexist with other UPnP services).

- **Periodic NOTIFY ssdp:alive** — one batch every `CACHE_MAX_AGE/4` ≈ 7 minutes, advertising `upnp:rootdevice`, the device UUID, `MediaServer:1`, `ContentDirectory:1`, `ConnectionManager:1`.
- **M-SEARCH responder** — unicast HTTP/1.1 200 OK to controllers that probe with `ST:` matching one of our targets (or `ssdp:all`).

The device UUID is `Uuid::new_v5(NAMESPACE, server_name)` so controllers see the same `uuid:` URN across launches even when the LAN IP changes — no on-disk persistence needed.

## Range streaming

`/stream/<track_id>` parses the `Range:` header, replies with `206 Partial Content` + `Content-Range`, and pipes the file through `tokio_util::io::ReaderStream::with_capacity(64 KiB)`. `take(length)` caps the reader so we never overshoot the window even if the controller closes early.

## Error handling

Failures inside the worker are surfaced through `DlnaStatus.last_error` so the Settings UI can display them inline:

- Bind failure (port in use, no privilege) → `"bind 0.0.0.0:1234: …"` and the server stays in the stopped state.
- SSDP socket failure → HTTP keeps serving for controllers that already know the LOCATION; only auto-discovery breaks. Surfaced as `"SSDP: …"`.

## Limitations / not implemented yet

- No `Search` action — `GetSearchCapabilities` returns an empty string so controllers fall back to Browse.
- No event subscriptions (`SUBSCRIBE` / `NOTIFY` from the event sub URLs). Controllers that need them will retry; nothing breaks.
- DSD (`.dsf` / `.dff`) doesn't appear in the audio MIME mapping — would need its own `audio/x-dsd` advertisement once the playback path lands.
- The bind is `0.0.0.0`. If you're on a public Wi-Fi network, **disable the toggle** — there's no auth.

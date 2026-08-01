# MPD protocol server

WaveFlow speaks the [MPD protocol](https://mpd.readthedocs.io/en/latest/protocol.html) on TCP, so any existing MPD client can drive the player: **MALP** / MaximumMPD on a phone, `mpc` and ncmpcpp in a terminal, waybar / polybar status modules, Home Assistant, shell scripts.

This is an adapter, not a new UI. MPD has been the Linux remote-control lingua franca since 2003; speaking it hands us a large parc of already-written clients — in particular **a phone remote without any mobile UI of our own** (issue #471).

Ships **disabled by default** — enable it from **Settings → Integrations → MPD server**.

## How it relates to the other control surfaces

|                                                                            | What it does                              |
| -------------------------------------------------------------------------- | ----------------------------------------- |
| [`media_controls`](../../src-tauri/crates/app/src/media_controls.rs) (MPRIS / SMTC) | OS media keys, **same machine**            |
| [DLNA MediaServer](dlna.md)                                                 | **serves** our files to a LAN receiver     |
| **MPD** (this page)                                                         | **controls** our player, from the LAN      |

## Architecture

Same dedicated-worker shape as [`dlna`](dlna.md): a sync `MpdServer` handle on `AppState` ferries `Cmd::{Start, Stop, Status}` over a crossbeam channel to the `mpd-worker` thread, which owns its own tokio runtime. Each accepted socket becomes a task.

```bash
AppState.mpd ─► Cmd channel ─► mpd-worker
                                ├─► TcpListener 0.0.0.0:6600 (scans 6600..6610)
                                │     └─► one task per client (connection.rs)
                                └─► Tauri event listeners ─► IdleBus
                                      player:state          ─► player
                                      player:track-changed  ─► player
                                      player:queue-changed  ─► playlist
```

### Why this is cheap here

WaveFlow's audio engine lives in Rust. The dispatcher reads [`SharedPlayback`](../../src-tauri/crates/app/src/audio/state.rs) atomics directly and drives playback through [`player_actions`](../../src-tauri/crates/app/src/player_actions.rs) — no request/response bridge to the webview, no timeout, no degraded mode, and **it keeps working with the window closed to tray**, which is exactly when a remote matters.

For contrast: a player whose audio lives in the webview (an `<audio>` element) has to ask the UI for its own playback state over an event round-trip, and an MPD `status` — which clients poll about once a second — turns into several of those.

### Shared player actions

`next` / `previous` / `play <pos>` go through [`player_actions`](../../src-tauri/crates/app/src/player_actions.rs), shared with the tray menu and the OS media controls. That sequence (advance the queue → `emit_track_changed` → `emit_queue_changed` → hand the track to the decoder) used to be copy-pasted in `lib.rs` and `media_controls.rs`; MPD would have made it a third copy, each free to forget an emit and desync a surface. Any new non-frontend control surface should call into that module rather than re-deriving it.

## Configuration

Persisted in the global `app_setting` table — the listener is process-wide, not per-profile.

| Key            | Default | Note                                                                                                     |
| -------------- | ------- | -------------------------------------------------------------------------------------------------------- |
| `mpd.enabled`  | `0`     | Opt-in. Auto-started at boot when set. **This flag is the security decision** — see below.                |
| `mpd.port`     | `6600`  | The MPD standard, which every client probes first. Scans forward through `6610` when taken.               |
| `mpd.password` | `""`    | Empty = no authentication.                                                                                |

## Bind address: `0.0.0.0`

The server binds every interface, matching [DLNA](dlna.md). Settled in #471 for two reasons.

**It matches what we already ship.** [`dlna/mod.rs`](../../src-tauri/crates/app/src/dlna/mod.rs) binds `0.0.0.0` and the DLNA HTTP layer has no authentication at all (UPnP has no such concept). WaveFlow therefore already exposes an opt-in, default-off, LAN-bound, zero-auth service — one that exposes strictly *more* than MPD control does:

|                            | DLNA | MPD |
| -------------------------- | ---- | --- |
| Enumerate the whole library | ✅   | ❌  |
| **Download the audio files** | ✅   | ❌  |
| See the current track       | ✅   | ✅  |
| Control playback            | ❌   | ✅  |
| Authentication              | none | optional password |

**On loopback the feature loses its point.** The phone remote is what justifies building this at all.

**Accepted risk:** unlike DLNA, MPD grants *write* access — anyone on the LAN can pause playback or change the volume. No file access, no shell, no exfiltration; the damage ceiling is "my music stopped". On a shared network (café, dorm, coworking) that is a real nuisance, and `mpd.password` is the answer — bearing in mind the protocol transmits it in cleartext, so it is a nuisance filter, not a security boundary.

Binding `0.0.0.0` triggers a firewall prompt on Windows/macOS the first time. DLNA already does this, so the behaviour is not new to users.

## Supported commands

Control and queue inspection. Advertised through `commands`, so clients hide UI for the rest.

| Group      | Commands                                                                            |
| ---------- | ----------------------------------------------------------------------------------- |
| Connection | `ping` · `close` · `password` · `commands` · `notcommands` · `tagtypes` · `urlhandlers` · `decoders` · `outputs` |
| State      | `status` · `currentsong` · `stats`                                                   |
| Queue read | `playlistinfo [range]` · `playlistid [id]`                                           |
| Transport  | `play [pos]` · `playid` · `pause [0/1]` · `stop` · `next` · `previous` · `seek` · `seekid` · `seekcur` |
| Mixer      | `setvol` · `getvol` · `volume`                                                        |
| Queue write| `clear` · `delete <range>` · `deleteid` · `move` · `moveid` · `shuffle`               |
| Options    | `random` · `repeat` · `single`                                                        |
| Idle       | `idle [subsystems]` · `noidle`                                                        |

Command lists (`command_list_begin` / `command_list_ok_begin` … `command_list_end`) are supported.

### Not implemented

- **Library browsing** — `lsinfo`, `search`, `find`, `add`, `listplaylists`. Deliberately deferred to keep the first cut reviewable. Unlike a streaming-only player we *do* have a local library to expose, and this is the piece that would make ncmpcpp genuinely useful (search an artist, queue it). Worth its own follow-up.
- **`consume`** — WaveFlow has no consume mode. `consume 0` is accepted, `consume 1` ACKs rather than silently lying.
- Stored-playlist mutation, stickers, partitions, multiple outputs.

## Mapping notes

**Song ids.** MPD's `Id` must be stable *per queue entry*, not per track — the same file can sit in the queue twice and `deleteid` / `moveid` must tell them apart. `queue_item.id` is an `INTEGER PRIMARY KEY` that survives reordering, so it maps directly. This is why [`mpd/songs.rs`](../../src-tauri/crates/app/src/mpd/songs.rs) has its own query instead of reusing `queue::list_queue`, which projects `track.id`.

**Repeat.** WaveFlow has a tri-state enum (`off` / `all` / `one`); MPD has two independent flags. `one` is `repeat 1` + `single 1`. Both setters preserve the other flag so a client toggling one doesn't clobber the other — see the round-trip test in [`mpd/commands.rs`](../../src-tauri/crates/app/src/mpd/commands.rs).

**Web Radio.** While a radio session owns the engine, `current_track_id` is a negative sentinel with no `track` row and no queue entry. `status` omits `song` / `songid` / `duration` and `currentsong` returns empty — same branch [`player_get_state`](../../src-tauri/crates/app/src/commands/player.rs) takes. Without it a client would show the last *library* track as if it were playing.

**`idle`.** Backed by the Tauri events the frontend already listens to, bridged onto an [`IdleBus`](../../src-tauri/crates/app/src/mpd/idle.rs). Only subsystems we actually fire are advertised (`player`, `playlist`, `mixer`, `options`, `output`) — claiming `database` would leave a client waiting on it forever. A burst is coalesced into one wake-up, so a track change answers once carrying both `player` and `playlist`.

## Trying it

```bash
# From the same machine
mpc -h 127.0.0.1 -p 6600 status
mpc -h 127.0.0.1 -p 6600 toggle

# From elsewhere on the LAN (address shown in Settings)
mpc -h 192.168.1.42 -p 6600 next
ncmpcpp -h 192.168.1.42

# With a password set
mpc -h 192.168.1.42 -P hunter2 status
```

On Android, point MALP at the address shown in Settings → Integrations → MPD server.

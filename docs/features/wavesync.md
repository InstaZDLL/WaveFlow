# WaveSync

Several WaveFlow apps on one network listen around one queue, without a
server: one hosts, the others join, and everybody adds tracks from their own
library. The design and its decisions are [RFC-010](../rfcs/RFC-010-wavesync.md);
this page says what is built.

**Lot 2 (this build): the session and the shared queue.** A participant plays
only the entries it owns, as ordinary library tracks; the others' entries are
listed with who added them, and are not playable here yet. Streaming them
from their owner and following the host's playback are lot 3.

## Where it lives

| Part                                         | What it does                                                                                                                                                                                                     |
| -------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `waveflow_core::wavesync`                    | The protocol, pure: the host as the only writer (`Host`), the replica each guest holds (`Replica`), the ops and their log, the snapshot, the rights, the control messages and the ranged-read frames, the clock. |
| `waveflow_core::lan` (feature `lan`)         | The LAN listener shared with RFC-008's phone link: the address filter, TLS with a certificate made on the machine, the code exchange, the tokens, mDNS.                                                          |
| `crates/app/src/wavesync/`                   | The app's half: hosting (`host.rs`), joining (`guest.rs`), this library's tracks as entries (`entries.rs`), the view the interface draws (`view.rs`).                                                            |
| `commands/wavesync.rs`                       | Thin commands; the view is also pushed as `wavesync:changed` after every change.                                                                                                                                 |
| `src/components/wavesync/`, `useWaveSync.ts` | The dialog, the panel, the notice; one subscription shared by every component.                                                                                                                                   |

## Starting and joining

The entry is always in the player bar's **⋯** menu (_WaveSync: listen
together…_). With no session it opens the dialog; with one it opens the
**WaveSync** panel, a right panel like the queue (a fourth tab in the Fluid
skin's side panel while a session runs).

- **Start a session.** A session name, the name the others see, and _Ask me
  before letting someone in_ (on by default). Starting makes a certificate,
  opens the listener on a port the system picks, on every IPv4 interface and
  IPv6 where the system has it, draws a code, and advertises the session over
  mDNS as `_wavesync._tcp` (TXT: protocol, certificate fingerprint, session
  name, host name; nothing about the library). Nothing listens before this,
  and everything stops when the session ends.
- **Join a session.** The dialog lists the sessions mDNS finds for 2.5 s
  (_Look again_ repeats it), or takes an address with its port
  (`192.168.1.20:43399`) or an invitation link
  (`wavesync://192.168.1.20:43399/?fp=<fingerprint>&code=<CODE>`), which the
  host's panel copies. Only an IP literal: a host name would be a DNS lookup
  nobody asked for. The guest types the code shown on the host's screen, read
  without case, dashes and spaces ignored.

Joining runs [the exchange](../architecture/crates.md): the code is never
sent, the host proves it first, a wrong code makes that address wait a
doubling delay, and the host's panel counts wrong codes. Then the host
approves (or turns away) the request, unless approval is off; the guest's
panel says it is waiting until then.

## The panel

- **Host:** the code in large type, _Draw a new code_ (the old one opens
  nothing from then on, even an exchange already under way), the certificate
  in four groups for whoever wants to compare, _Copy invitation_, the requests
  to join, the wrong codes tried, and, after a minute with nobody in, the
  firewall hint (_Nobody can join? Check that this network is set as private
  and that WaveFlow is allowed through the firewall_).
- **Participants:** names, the host marked, _away_ for a guest whose
  connection dropped; the host can remove a guest.
- **Queue:** every entry with its credits, who added it, and its state:
  played, the one playing, upcoming, _owner away_. One's own entries have a
  play button, which plays the library track here (it counts in history and
  scrobbles as any library track, decision 9). The host can remove any entry,
  a guest its own. Virtualized, in the panel's own scroll, like the queue
  panel.
- **Leave** / **End the session**.

When a session ends, the panel says why: ended, left, ended by the host,
connection lost, removed, not let in, offline mode, another profile, or out
of step with the host.

## Adding tracks

_Add to WaveSync_ appears in a track's context menu and in the selection bar
while a session runs and this machine is in it. Entries are built from the
library rows: title, credits (`track_artist`, one name each), album,
duration, codec, sample rate, channels, ReplayGain, and the file's size read
from the disk. A track is skipped, and the notice counts it, when it is on a
network share, is DSD, is over 2 GiB, or whose file is gone (decision 10).
Guests add at the end of the queue; the host's adds go there too from the
interface.

## Rules that hold

- **Offline mode ends a session** and refuses starting, joining and
  discovery (decision 8): `set_offline_mode` calls
  `wavesync::offline_changed`.
- **A profile switch ends it** before the pool changes: the entries are the
  old profile's tracks (`switch_profile`, `deactivate_profile`).
- **Nothing secret reaches the log**: lines name a participant by its number
  and never print the code, a token, an invitation, a name or a title.
- **No CSP change**: everything crosses in the backend; the webview invokes
  commands and listens to `wavesync:changed`.
- **Windows** asks, the first time a session is started, whether WaveFlow may
  accept connections; on a network classed _Public_, refused connections are
  silent, hence the hint. **macOS**: `NSLocalNetworkUsageDescription` names
  WaveSync. **Flatpak**: `--share=network` is enough; the mDNS responder is the
  app's own.

## Not in this lot

Streaming another participant's entry and following the host (lot 3),
gapless and the listener's own identical file (lot 4), reconnection with the
token, hand-over of hosting and the private-VPN switch (lot 5). A guest whose
connection drops is _away_ and has to join again.

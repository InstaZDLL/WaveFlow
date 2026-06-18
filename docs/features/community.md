# Community-DB

> **Status — placeholder.** This page is the user-facing companion to [RFC-004 — Community-DB](../rfcs/RFC-004-community-database.md). It's stubbed during the RFC's Draft phase so contributors can link to a stable URL while the feature lands. Real copy fills in during Phase 2.a (server endpoints + Settings → Community panel). See the RFC for the actual data model, schema, API surface, and privacy boundary.

## What this will be

An **opt-in, federated** pool of crowdsourced metadata — lyrics, artist bios, album corrections, BPM + musical key + audio features — shared across `waveflow-server` instances that point at the same mirror. Modelled on [LRCLIB](https://lrclib.net): anonymous reads, JWT-authed writes, vote-based moderation. Self-hostable end-to-end; no mandatory dependency on any centralised service.

The three knobs that ship in Settings → Community :

| Knob                          | Default | Effect                                                              |
| ----------------------------- | ------- | ------------------------------------------------------------------- |
| **Look up community DB**      | `on`    | Consult community DB in the metadata fallback chain. No identity leaks. |
| **Contribute corrections**    | `off`   | Push lyric / bio / metadata corrections upstream (requires JWT).    |
| **Mirror URL**                | unset   | Which public mirror to pull from. Unset = private pool only.        |

Default deployment = lookup enabled, contribute disabled, mirror unset → behaviour is **identical to today's build** until the user opts in.

## Where it sits in the fallback chain

Community DB lands **last** before "empty" — `Deezer / Last.fm / LRCLIB` keep their current ranking. The full chain after Phase 2.b ships :

```text
embedded tags
    → local cache
    → Deezer / Last.fm / LRCLIB  (parallel, first non-empty wins)
    → community DB               (new — last before empty)
    → empty
```

Rationale lives in [RFC-004 §8](../rfcs/RFC-004-community-database.md).

## Privacy

Three commitments encoded into the protocol (full detail in [RFC-004 §9](../rfcs/RFC-004-community-database.md)) :

- **No listener identity in the contribution stream.** Entity keys are content-derived BLAKE3 hashes, not per-user signatures.
- **No play history.** Listening behaviour stays in the per-user sync stream (Phase 1.f). Deliberately separate.
- **No reverse-lookup of "does user X have track Y".** Lookup is by hash; the server cannot reverse a lookup into a library inventory.

## What lands when

Implementation gated by the 1.5.0 cut. Phases per RFC-004 §11 :

- **2.a** Server schema + `/lookup` + `/contribute` + `/vote` (lyrics first)
- **2.b** Desktop integration into the fallback chain. Settings → Community panel.
- **2.c** Moderation queue + moderator role. Audio features + album metadata payload kinds.
- **2.d** Federated pulls (one mirror at a time). Cover-art payload kind via the artwork pipeline.

## See also

- [RFC-004 — Community-DB](../rfcs/RFC-004-community-database.md) — the full data model + API surface + privacy boundary
- [RFC-001 — WaveFlow Server](../rfcs/RFC-001-waveflow-server.md) — community DB runs on the same server + auth pipeline
- [Integrations](integrations.md) — current Deezer / Last.fm / LRCLIB clients that community DB complements

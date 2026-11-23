//! Whether the sound server just moved our stream off a device that went
//! away (#1158). Pure: it reads snapshots of the server's state and keeps
//! no handle on the server, so every rule here is tested without one.
//!
//! Measured on PipeWire 1.6 (pipewire-pulse, WirePlumber 0.5) with an
//! ALSA `default` stream — what cpal opens:
//!
//! - **A sink that disappears** (a USB headset unplugged, a Bluetooth one
//!   switched off): the server removes the sink and moves the stream to
//!   its new default. The stream itself sees nothing — no error, no
//!   underrun, writes keep succeeding.
//! - **The default changing while the old device stays** (picked in the
//!   system settings), and **the stream moved by hand** (a per-application
//!   output picked in the system mixer): the same move, with no sink
//!   removed.
//! - **A 3.5 mm jack on a built-in card** is not a sink at all: the card
//!   has one sink with a speaker port and a headphone port, and unplugging
//!   marks the headphone port unavailable and makes the speaker active. No
//!   sink comes or goes, and the stream does not move.
//!
//! So a move alone says nothing, and a missing move does not mean nothing
//! happened. What separates a device going away from a choice someone
//! made is whether what we were playing through is still there:
//!
//! - the stream left a sink that no longer exists, and the card it
//!   belonged to went with it (or it had none) — [`Loss::SinkGone`];
//! - it left a sink that no longer exists on a card that is still here,
//!   but the port it played through is now unavailable — an HDMI or
//!   jack port unplugged, after which the server switched the card's
//!   profile — [`Loss::PortUnplugged`]. The same sink replaced while its
//!   port stays usable is a profile switch (a Bluetooth headset going to
//!   its call profile, an output picked in the system settings): nothing
//!   was lost;
//! - it stayed on the same sink, and the port that sink was playing
//!   through became unavailable — the jack case — [`Loss::PortUnplugged`].
//!
//! A port only counts once: one that was already unavailable when we first
//! saw it (a sink whose active port the server never switched) is never
//! "lost" later, or every event would stop the music again.

use std::collections::HashMap;

/// A port's availability, as the server reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    /// Jack detection says something is plugged in.
    Available,
    /// Jack detection says nothing is: the port cannot be heard.
    Unavailable,
    /// The port has no jack detection — a laptop's speakers, most often.
    Unknown,
}

/// One sink of the server, reduced to what the decision reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkView {
    pub index: u32,
    /// The card it belongs to; `None` for a virtual or network sink.
    pub card: Option<u32>,
    /// The port it plays through; `None` for a sink without ports.
    pub active_port: Option<String>,
    pub ports: Vec<(String, PortState)>,
}

/// One card of the server, reduced to its ports' availability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardView {
    pub index: u32,
    pub ports: Vec<(String, PortState)>,
}

/// One of our own streams: its index on the server and the sink it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamView {
    pub index: u32,
    pub sink: u32,
}

/// The server's state at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub sinks: Vec<SinkView>,
    /// Every card [`Tracker::cards_in_question`] named that still exists;
    /// any other card may be left out, and a missing one counts as gone.
    /// A card that exists but could not be read is here with no ports,
    /// which never makes a loss.
    pub cards: Vec<CardView>,
    /// Our process's streams only. Usually one; two for the instant an
    /// output rebuild has opened the new stream before closing the old.
    pub ours: Vec<StreamView>,
}

impl Snapshot {
    fn sink(&self, index: u32) -> Option<&SinkView> {
        self.sinks.iter().find(|sink| sink.index == index)
    }

    fn card(&self, index: u32) -> Option<&CardView> {
        self.cards.iter().find(|card| card.index == index)
    }
}

/// Why the device under our stream counts as gone. Logged, and what the
/// tests assert on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Loss {
    /// The sink went away with its card, or had none.
    SinkGone,
    /// The port the stream played through became unavailable.
    PortUnplugged,
}

/// Where one of our streams was last seen playing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    sink: u32,
    card: Option<u32>,
    /// The sink's active port, kept only while it could be heard: a port
    /// already unavailable when seen must not count as lost later.
    port: Option<String>,
}

impl Seen {
    fn of(sink: &SinkView) -> Self {
        let port = sink
            .active_port
            .clone()
            .filter(|port| port_state(&sink.ports, port) != Some(PortState::Unavailable));
        Self {
            sink: sink.index,
            card: sink.card,
            port,
        }
    }
}

fn port_state(ports: &[(String, PortState)], name: &str) -> Option<PortState> {
    ports
        .iter()
        .find(|(port, _)| port == name)
        .map(|(_, state)| *state)
}

/// Follows our streams from one snapshot to the next.
///
/// Keyed on the stream's index, not on "our stream": an output WaveFlow
/// rebuilds — a device picked in the app, the exclusive toggle, a
/// recovery — closes the stream and opens another under a new index. The
/// new one starts with no history, so a change the user made in WaveFlow
/// is never judged as a move.
#[derive(Debug, Default)]
pub struct Tracker {
    seen: HashMap<u32, Seen>,
}

impl Tracker {
    /// The cards the next [`Self::observe`] will look at: those of the
    /// sinks our streams were on that `snapshot` no longer lists. Asked
    /// before the cards are read, so only these are: a card's ports are
    /// the one reply the server sends that we may fail to decode, and
    /// reading all of them would let one card stop the watch for every
    /// other.
    pub fn cards_in_question(&self, snapshot: &Snapshot) -> Vec<u32> {
        let mut cards: Vec<u32> = self
            .seen
            .values()
            .filter(|seen| snapshot.sink(seen.sink).is_none())
            .filter_map(|seen| seen.card)
            .collect();
        cards.sort_unstable();
        cards.dedup();
        cards
    }

    /// Read the next snapshot. `Some` when one of our streams lost the
    /// device it was playing through since the previous one.
    pub fn observe(&mut self, snapshot: &Snapshot) -> Option<Loss> {
        let mut loss = None;
        let mut next = HashMap::with_capacity(snapshot.ours.len());
        for stream in &snapshot.ours {
            let previous = self.seen.get(&stream.index);
            let Some(sink) = snapshot.sink(stream.sink) else {
                // The stream names a sink the server no longer lists: it
                // is between the removal and the move. Keep what we knew
                // and judge on the next snapshot, once it has landed.
                if let Some(previous) = previous {
                    next.insert(stream.index, previous.clone());
                }
                continue;
            };
            if let Some(previous) = previous {
                loss = loss.or_else(|| judge(previous, sink, snapshot));
            }
            next.insert(stream.index, Seen::of(sink));
        }
        // A stream missing from the snapshot was closed; its history goes
        // with it.
        self.seen = next;
        loss
    }
}

fn judge(previous: &Seen, now: &SinkView, snapshot: &Snapshot) -> Option<Loss> {
    let port_gone = |ports: &[(String, PortState)]| {
        previous
            .port
            .as_deref()
            .is_some_and(|port| port_state(ports, port) == Some(PortState::Unavailable))
    };
    if previous.sink == now.index {
        // Same sink. A port switched by hand leaves the old one usable;
        // a jack pulled out does not.
        return port_gone(&now.ports).then_some(Loss::PortUnplugged);
    }
    if snapshot.sink(previous.sink).is_some() {
        // Moved, and the old sink is still there: the default changed or
        // someone moved the stream. A choice, not a loss.
        return None;
    }
    match previous.card.map(|card| snapshot.card(card)) {
        // No card, or the card went too: the device itself is gone.
        None | Some(None) => Some(Loss::SinkGone),
        // The card stayed and only its sink was replaced: a profile
        // switch, a loss only if the port we played through went dead.
        Some(Some(card)) => port_gone(&card.ports).then_some(Loss::PortUnplugged),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEAKER: &str = "analog-output-speaker";
    const HEADPHONES: &str = "analog-output-headphones";

    fn ports(list: &[(&str, PortState)]) -> Vec<(String, PortState)> {
        list.iter()
            .map(|(name, state)| ((*name).to_owned(), *state))
            .collect()
    }

    fn sink(
        index: u32,
        card: Option<u32>,
        active: Option<&str>,
        list: &[(&str, PortState)],
    ) -> SinkView {
        SinkView {
            index,
            card,
            active_port: active.map(str::to_owned),
            ports: ports(list),
        }
    }

    /// The built-in card of the measured laptop: one sink, two ports.
    fn builtin(active: &str, headphones: PortState) -> SinkView {
        sink(
            53,
            Some(43),
            Some(active),
            &[(SPEAKER, PortState::Unknown), (HEADPHONES, headphones)],
        )
    }

    fn card(index: u32, list: &[(&str, PortState)]) -> CardView {
        CardView {
            index,
            ports: ports(list),
        }
    }

    fn ours(index: u32, sink: u32) -> StreamView {
        StreamView { index, sink }
    }

    fn snapshot(sinks: Vec<SinkView>, cards: Vec<CardView>, streams: Vec<StreamView>) -> Snapshot {
        Snapshot {
            sinks,
            cards,
            ours: streams,
        }
    }

    /// The measured case: a sink without a card (a null sink standing in
    /// for a USB headset) removed under a playing stream, which the
    /// server moved to the built-in sink.
    #[test]
    fn a_sink_removed_under_the_stream_is_a_loss() {
        let mut tracker = Tracker::default();
        let headset = sink(91, None, None, &[]);
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        let before = snapshot(vec![headset, speakers.clone()], vec![], vec![ours(99, 91)]);
        assert_eq!(tracker.observe(&before), None);
        let after = snapshot(vec![speakers], vec![], vec![ours(99, 53)]);
        assert_eq!(tracker.observe(&after), Some(Loss::SinkGone));
    }

    /// The removal is seen before the move: the stream still names the
    /// sink that has just gone. Nothing is decided until it lands.
    #[test]
    fn a_stream_between_removal_and_move_is_judged_once_it_lands() {
        let mut tracker = Tracker::default();
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        let usb = sink(
            91,
            Some(70),
            Some("analog-output"),
            &[("analog-output", PortState::Unknown)],
        );
        let usb_card = card(70, &[("analog-output", PortState::Unknown)]);
        let builtin_card = card(43, &[]);
        tracker.observe(&snapshot(
            vec![usb, speakers.clone()],
            vec![usb_card, builtin_card.clone()],
            vec![ours(99, 91)],
        ));
        let removed = snapshot(
            vec![speakers.clone()],
            vec![builtin_card.clone()],
            vec![ours(99, 91)],
        );
        assert_eq!(tracker.observe(&removed), None);
        let moved = snapshot(vec![speakers], vec![builtin_card], vec![ours(99, 53)]);
        assert_eq!(tracker.observe(&moved), Some(Loss::SinkGone));
    }

    /// Measured: picking another default in the system settings moves the
    /// stream, and removes nothing.
    #[test]
    fn a_default_that_changes_is_not_a_loss() {
        let mut tracker = Tracker::default();
        let a = sink(115, None, None, &[]);
        let b = sink(122, None, None, &[]);
        tracker.observe(&snapshot(
            vec![a.clone(), b.clone()],
            vec![],
            vec![ours(129, 115)],
        ));
        let moved = snapshot(vec![a.clone(), b.clone()], vec![], vec![ours(129, 122)]);
        assert_eq!(tracker.observe(&moved), None);
        // And moved back by hand from the system mixer.
        let back = snapshot(vec![a, b], vec![], vec![ours(129, 115)]);
        assert_eq!(tracker.observe(&back), None);
    }

    /// The reported case on a built-in card: the headphone jack pulled,
    /// the server marks the port unavailable and switches to the
    /// speakers, on the same sink.
    #[test]
    fn a_headphone_jack_pulled_out_is_a_loss() {
        let mut tracker = Tracker::default();
        let plugged = snapshot(
            vec![builtin(HEADPHONES, PortState::Available)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&plugged), None);
        let pulled = snapshot(
            vec![builtin(SPEAKER, PortState::Unavailable)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&pulled), Some(Loss::PortUnplugged));
        // Said once: the next event on the speakers is not a new loss.
        assert_eq!(tracker.observe(&pulled), None);
    }

    /// The port marked unavailable before the server switches away from
    /// it: the loss is the port, not the switch.
    #[test]
    fn a_port_unavailable_before_the_switch_is_a_loss() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(
            vec![builtin(HEADPHONES, PortState::Available)],
            vec![],
            vec![ours(99, 53)],
        ));
        let dead = snapshot(
            vec![builtin(HEADPHONES, PortState::Unavailable)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&dead), Some(Loss::PortUnplugged));
        let switched = snapshot(
            vec![builtin(SPEAKER, PortState::Unavailable)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&switched), None);
    }

    /// Switching the port by hand leaves the old one usable.
    #[test]
    fn a_port_switched_by_hand_is_not_a_loss() {
        let mut tracker = Tracker::default();
        tracker.observe(&snapshot(
            vec![builtin(HEADPHONES, PortState::Available)],
            vec![],
            vec![ours(99, 53)],
        ));
        let switched = snapshot(
            vec![builtin(SPEAKER, PortState::Available)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&switched), None);
    }

    /// A port that was already dead when first seen is not lost later.
    #[test]
    fn a_port_already_unavailable_never_counts() {
        let mut tracker = Tracker::default();
        let stuck = snapshot(
            vec![builtin(HEADPHONES, PortState::Unavailable)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&stuck), None);
        assert_eq!(tracker.observe(&stuck), None);
        let switched = snapshot(
            vec![builtin(SPEAKER, PortState::Unavailable)],
            vec![],
            vec![ours(99, 53)],
        );
        assert_eq!(tracker.observe(&switched), None);
    }

    /// A Bluetooth headset switching to its call profile replaces its sink
    /// on a card that stays, and its port stays usable.
    #[test]
    fn a_profile_switch_on_a_card_that_stays_is_not_a_loss() {
        let mut tracker = Tracker::default();
        let a2dp = sink(
            140,
            Some(80),
            Some("headset-output"),
            &[("headset-output", PortState::Unknown)],
        );
        let hfp = sink(
            141,
            Some(80),
            Some("headset-hf-output"),
            &[("headset-hf-output", PortState::Unknown)],
        );
        let bt = card(
            80,
            &[
                ("headset-output", PortState::Unknown),
                ("headset-hf-output", PortState::Unknown),
            ],
        );
        tracker.observe(&snapshot(
            vec![a2dp],
            vec![bt.clone()],
            vec![ours(150, 140)],
        ));
        let switched = snapshot(vec![hfp], vec![bt], vec![ours(150, 141)]);
        assert_eq!(tracker.observe(&switched), None);
    }

    /// An HDMI cable pulled: the port goes dead, the server switches the
    /// card to another profile, which replaces the sink.
    #[test]
    fn a_sink_replaced_after_its_port_died_is_a_loss() {
        let mut tracker = Tracker::default();
        let hdmi = sink(
            60,
            Some(43),
            Some("hdmi-output-0"),
            &[("hdmi-output-0", PortState::Available)],
        );
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        let plugged = card(43, &[("hdmi-output-0", PortState::Available)]);
        tracker.observe(&snapshot(vec![hdmi], vec![plugged], vec![ours(99, 60)]));
        let pulled = card(43, &[("hdmi-output-0", PortState::Unavailable)]);
        let after = snapshot(vec![speakers], vec![pulled], vec![ours(99, 53)]);
        assert_eq!(tracker.observe(&after), Some(Loss::PortUnplugged));
    }

    /// A USB device unplugged: the sink goes, and its card with it.
    #[test]
    fn a_sink_gone_with_its_card_is_a_loss() {
        let mut tracker = Tracker::default();
        let usb = sink(
            91,
            Some(70),
            Some("analog-output"),
            &[("analog-output", PortState::Unknown)],
        );
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        tracker.observe(&snapshot(
            vec![usb, speakers.clone()],
            vec![card(70, &[]), card(43, &[])],
            vec![ours(99, 91)],
        ));
        let after = snapshot(vec![speakers], vec![card(43, &[])], vec![ours(99, 53)]);
        assert_eq!(tracker.observe(&after), Some(Loss::SinkGone));
    }

    /// An output picked in WaveFlow closes our stream and opens another
    /// under a new index, on another sink: no history, no verdict — even
    /// when the old sink then disappears.
    #[test]
    fn a_new_stream_of_ours_is_never_judged_against_the_old_one() {
        let mut tracker = Tracker::default();
        let a = sink(115, None, None, &[]);
        let b = sink(122, None, None, &[]);
        tracker.observe(&snapshot(vec![a, b.clone()], vec![], vec![ours(129, 115)]));
        let reopened = snapshot(vec![b], vec![], vec![ours(130, 122)]);
        assert_eq!(tracker.observe(&reopened), None);
    }

    /// A stream that closes forgets its history, so an index the server
    /// reuses later starts afresh.
    #[test]
    fn a_closed_stream_is_forgotten() {
        let mut tracker = Tracker::default();
        let a = sink(115, None, None, &[]);
        let b = sink(122, None, None, &[]);
        tracker.observe(&snapshot(vec![a, b.clone()], vec![], vec![ours(129, 115)]));
        tracker.observe(&snapshot(vec![b.clone()], vec![], vec![]));
        let reused = snapshot(vec![b], vec![], vec![ours(129, 122)]);
        assert_eq!(tracker.observe(&reused), None);
    }

    /// Only the cards of sinks that left are asked about, once each.
    #[test]
    fn only_the_cards_of_sinks_that_left_are_in_question() {
        let mut tracker = Tracker::default();
        let usb = sink(91, Some(70), None, &[]);
        let virtual_sink = sink(92, None, None, &[]);
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        let all = vec![usb, virtual_sink, speakers.clone()];
        tracker.observe(&snapshot(
            all.clone(),
            vec![],
            vec![ours(98, 91), ours(99, 91), ours(100, 92), ours(101, 53)],
        ));
        assert!(tracker
            .cards_in_question(&snapshot(all, vec![], vec![]))
            .is_empty());
        let after = snapshot(vec![speakers], vec![], vec![]);
        assert_eq!(tracker.cards_in_question(&after), vec![70]);
    }

    /// A card that is still there but whose ports could not be read is
    /// not a reason to stop the music.
    #[test]
    fn an_unreadable_card_that_stays_is_not_a_loss() {
        let mut tracker = Tracker::default();
        let hdmi = sink(
            60,
            Some(43),
            Some("hdmi-output-0"),
            &[("hdmi-output-0", PortState::Available)],
        );
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        tracker.observe(&snapshot(vec![hdmi], vec![], vec![ours(99, 60)]));
        let after = snapshot(vec![speakers], vec![card(43, &[])], vec![ours(99, 53)]);
        assert_eq!(tracker.observe(&after), None);
    }

    /// While a rebuild holds two of our streams, the one that lost its
    /// device is still caught.
    #[test]
    fn a_loss_on_either_of_two_streams_counts() {
        let mut tracker = Tracker::default();
        let gone = sink(91, None, None, &[]);
        let speakers = builtin(SPEAKER, PortState::Unavailable);
        tracker.observe(&snapshot(
            vec![gone, speakers.clone()],
            vec![],
            vec![ours(100, 53), ours(99, 91)],
        ));
        let after = snapshot(vec![speakers], vec![], vec![ours(100, 53), ours(99, 53)]);
        assert_eq!(tracker.observe(&after), Some(Loss::SinkGone));
    }
}

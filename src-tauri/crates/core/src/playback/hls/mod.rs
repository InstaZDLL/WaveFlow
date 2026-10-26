//! HTTP Live Streaming for radio: the playlists ([`playlist`]) and the
//! audio out of their segments ([`ts`]). The polling and the network are
//! the player's ([`crate::playback::timeshift`] is where the audio lands).

pub mod playlist;
pub mod ts;

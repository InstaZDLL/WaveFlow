//! Aggregate "browse" queries for a library: albums, artists, genres, folders.
//!
//! These commands back the Albums / Artistes / Genres / Dossiers tabs in the
//! library view. They all take a `library_id` and filter content to rows that
//! have at least one available track in that library — important because
//! `album`, `artist` and `genre` are profile-wide tables shared across
//! libraries.

mod albums;
mod artists;
mod detail;
mod folders;
mod genres;
mod history;
mod library_albums;
mod library_artists;
mod library_sql;
mod library_tracks;

pub use albums::*;
pub use artists::*;
pub use detail::*;
pub use folders::*;
pub use genres::*;
pub use history::*;
pub use library_albums::*;
pub use library_artists::*;
use library_sql::*;
pub use library_tracks::*;

#[cfg(test)]
mod tests;

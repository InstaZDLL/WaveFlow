//! Library DTOs. A library is a collection of scanned audio folders
//! tagged with a colour + icon. Each profile can host many libraries
//! ("Bandes-son", "Live", "Démos", …) — they materialise as the
//! sidebar shelves in the desktop UI.

use serde::{Deserialize, Serialize};

/// Library row returned to the frontend, with denormalised counts
/// computed on the fly so the sidebar can display "X titres · Y albums"
/// without issuing a second query per library.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "sqlite", derive(sqlx::FromRow))]
pub struct Library {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub color_id: String,
    pub icon_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub track_count: i64,
    pub album_count: i64,
    pub artist_count: i64,
    pub genre_count: i64,
    pub folder_count: i64,
}

#[derive(Debug, Deserialize)]
pub struct CreateLibraryInput {
    pub name: String,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

/// Partial update payload — any field left as `None` is preserved via
/// SQL `COALESCE`. The description cannot be cleared through this shape,
/// which is fine for the current UX.
#[derive(Debug, Deserialize)]
pub struct UpdateLibraryInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color_id: Option<String>,
    pub icon_id: Option<String>,
}

//! Persistence operations for album metadata derived from scanned tracks.

use async_trait::async_trait;

use crate::error::CoreResult;

#[async_trait]
pub trait AlbumRepository: Send + Sync {
    /// Recompute years for albums currently linked to `folder_id` and for
    /// albums that held tracks before this scan reassigned them. Returns the
    /// number of album rows whose year changed.
    async fn refresh_years_after_folder_scan(
        &self,
        folder_id: i64,
        previous_album_ids: &[i64],
    ) -> CoreResult<u64>;
}

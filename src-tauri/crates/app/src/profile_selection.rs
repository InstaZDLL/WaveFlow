use crate::error::AppResult;
use sqlx::SqlitePool;

pub(crate) async fn resolve_target_profile(app_db: &SqlitePool) -> AppResult<Option<i64>> {
    let last_profile_id: Option<String> =
        sqlx::query_scalar("SELECT value FROM app_setting WHERE key = 'app.last_profile_id'")
            .fetch_optional(app_db)
            .await?;

    if let Some(id_str) = last_profile_id {
        if let Ok(id) = id_str.parse::<i64>() {
            let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM profile WHERE id = ?")
                .bind(id)
                .fetch_optional(app_db)
                .await?;
            if exists.is_some() {
                return Ok(Some(id));
            }
        }
    }

    let fallback: Option<i64> =
        sqlx::query_scalar("SELECT id FROM profile ORDER BY last_used_at DESC LIMIT 1")
            .fetch_optional(app_db)
            .await?;

    Ok(fallback)
}

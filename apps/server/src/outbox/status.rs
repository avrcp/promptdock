use sqlx::Row as _;
use uuid::Uuid;

use super::{
    ChannelOutboxStatus, NotificationStatus, OutboxError, OutboxService, repository::database_error,
};

impl OutboxService {
    pub async fn status(
        &self,
        device_id: Uuid,
        notification_id: &str,
    ) -> Result<NotificationStatus, OutboxError> {
        if notification_id.is_empty() || notification_id.chars().count() > 128 {
            return Err(OutboxError::NotFound);
        }
        let row = sqlx::query(
            "SELECT notification_id, status, attempt_count, last_error_code,
                    provider_message_id, updated_at, provider_accepted_at
             FROM notification_outbox
             WHERE origin_kind = 'device' AND origin_device_id = ?1
               AND origin_key = 'device:' || ?1
               AND notification_id = ?2",
        )
        .bind(device_id.to_string())
        .bind(notification_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(database_error)?
        .ok_or(OutboxError::NotFound)?;
        status_from_row(&row)
    }

    pub async fn channel_status(&self) -> Result<ChannelOutboxStatus, OutboxError> {
        let row = sqlx::query(
            "SELECT
                COALESCE(SUM(CASE WHEN status IN (
                    'pending_channel', 'sending_channel', 'retry_wait'
                ) THEN 1 ELSE 0 END), 0) AS pending_notifications,
                COALESCE(SUM(CASE WHEN status IN (
                    'blocked_activation', 'blocked_reconnect'
                ) THEN 1 ELSE 0 END), 0) AS blocked_notifications,
                MAX(provider_accepted_at) AS last_provider_accepted_at
             FROM notification_outbox",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(database_error)?;
        Ok(ChannelOutboxStatus {
            pending_notifications: row
                .try_get("pending_notifications")
                .map_err(database_error)?,
            blocked_notifications: row
                .try_get("blocked_notifications")
                .map_err(database_error)?,
            last_provider_accepted_at: row
                .try_get("last_provider_accepted_at")
                .map_err(database_error)?,
        })
    }
}

fn status_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<NotificationStatus, OutboxError> {
    Ok(NotificationStatus {
        notification_id: row.try_get("notification_id").map_err(database_error)?,
        status: row.try_get("status").map_err(database_error)?,
        attempt_count: row.try_get("attempt_count").map_err(database_error)?,
        last_error_code: row.try_get("last_error_code").map_err(database_error)?,
        provider_message_id: row.try_get("provider_message_id").map_err(database_error)?,
        updated_at: row.try_get("updated_at").map_err(database_error)?,
        provider_accepted_at: row
            .try_get("provider_accepted_at")
            .map_err(database_error)?,
    })
}

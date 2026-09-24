use sqlx::{Row as _, Sqlite, Transaction};
use uuid::Uuid;

use super::{
    AcceptedNotification, AdminTest, InteractiveReplyV1, OutboxError, OutboxService,
    RelayNotificationV1,
    repository::{database_error, unix_timestamp_ms},
    validation::{validate_notification, validate_target_account_fingerprint},
};

const PAYLOAD_HASH_CONTEXT: &str = "promptdock-relay/notification-payload/v1";
const SYSTEM_INTERACTIVE_ORIGIN_KEY: &str = "system:interactive";
const ADMIN_ORIGIN_KEY: &str = "admin:singleton";
const ADMIN_TEST_TTL_MS: i64 = 5 * 60 * 1_000;
const ADMIN_TEST_TITLE: &str = "PromptDock Relay 测试";
const ADMIN_TEST_BODY: &str = "✅ PromptDock Relay 测试通知";

enum OutboxOrigin {
    Device { key: String, device_id: String },
    SystemInteractive,
    Admin,
}

impl OutboxOrigin {
    fn device(device_id: Uuid) -> Self {
        let device_id = device_id.to_string();
        Self::Device {
            key: format!("device:{device_id}"),
            device_id,
        }
    }

    const fn kind(&self) -> &'static str {
        match self {
            Self::Device { .. } => "device",
            Self::SystemInteractive => "system",
            Self::Admin => "admin",
        }
    }

    fn key(&self) -> &str {
        match self {
            Self::Device { key, .. } => key,
            Self::SystemInteractive => SYSTEM_INTERACTIVE_ORIGIN_KEY,
            Self::Admin => ADMIN_ORIGIN_KEY,
        }
    }

    fn device_id(&self) -> Option<&str> {
        match self {
            Self::Device { device_id, .. } => Some(device_id),
            Self::SystemInteractive | Self::Admin => None,
        }
    }
}

impl OutboxService {
    pub async fn enqueue_device(
        &self,
        device_id: Uuid,
        notification: RelayNotificationV1,
    ) -> Result<AcceptedNotification, OutboxError> {
        self.enqueue_device_at(device_id, notification, unix_timestamp_ms()?)
            .await
    }

    pub async fn enqueue_system_reply(
        &self,
        reply: InteractiveReplyV1,
    ) -> Result<AcceptedNotification, OutboxError> {
        self.enqueue_system_reply_at(reply, unix_timestamp_ms()?)
            .await
    }

    pub async fn enqueue_admin_test(
        &self,
        _test: AdminTest,
    ) -> Result<AcceptedNotification, OutboxError> {
        self.enqueue_admin_test_at(unix_timestamp_ms()?).await
    }

    pub(super) async fn enqueue_device_at(
        &self,
        device_id: Uuid,
        notification: RelayNotificationV1,
        accepted_at: i64,
    ) -> Result<AcceptedNotification, OutboxError> {
        if notification.kind == "interactive_reply" {
            return Err(OutboxError::Validation);
        }
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let accepted = stage_origin_in(
            &mut transaction,
            OutboxOrigin::device(device_id),
            notification,
            None,
            false,
            accepted_at,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if !accepted.existing {
            self.notify_worker();
        }
        Ok(accepted)
    }

    pub(super) async fn enqueue_system_reply_at(
        &self,
        reply: InteractiveReplyV1,
        accepted_at: i64,
    ) -> Result<AcceptedNotification, OutboxError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let accepted = self
            .stage_system_reply_in(&mut transaction, reply, accepted_at)
            .await?;
        transaction.commit().await.map_err(database_error)?;
        if !accepted.existing {
            self.notify_worker();
        }
        Ok(accepted)
    }

    pub(super) async fn enqueue_admin_test_at(
        &self,
        accepted_at: i64,
    ) -> Result<AcceptedNotification, OutboxError> {
        let notification_id = format!("admin-test-{}", Uuid::new_v4());
        let notification = RelayNotificationV1 {
            schema_version: 1,
            notification_id: notification_id.clone(),
            dedupe_key: notification_id,
            kind: "test".to_owned(),
            priority: 100,
            title: ADMIN_TEST_TITLE.to_owned(),
            body: ADMIN_TEST_BODY.to_owned(),
            correlation_key: None,
            created_at: accepted_at,
            expires_at: accepted_at.saturating_add(ADMIN_TEST_TTL_MS),
        };
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        let accepted = stage_origin_in(
            &mut transaction,
            OutboxOrigin::Admin,
            notification,
            None,
            false,
            accepted_at,
        )
        .await?;
        transaction.commit().await.map_err(database_error)?;
        if !accepted.existing {
            self.notify_worker();
        }
        Ok(accepted)
    }

    pub(crate) async fn stage_system_reply_in(
        &self,
        transaction: &mut Transaction<'_, Sqlite>,
        reply: InteractiveReplyV1,
        accepted_at: i64,
    ) -> Result<AcceptedNotification, OutboxError> {
        let (notification, target_account_fingerprint, sensitive_body) = reply.into_parts();
        validate_target_account_fingerprint(&target_account_fingerprint)?;
        stage_origin_in(
            transaction,
            OutboxOrigin::SystemInteractive,
            notification,
            Some(target_account_fingerprint),
            sensitive_body,
            accepted_at,
        )
        .await
    }

    pub(crate) fn notify_worker(&self) {
        self.wake.notify_one();
    }
}

async fn stage_origin_in(
    transaction: &mut Transaction<'_, Sqlite>,
    origin: OutboxOrigin,
    notification: RelayNotificationV1,
    target_account_fingerprint: Option<String>,
    sensitive_body: bool,
    accepted_at: i64,
) -> Result<AcceptedNotification, OutboxError> {
    validate_notification(&notification, accepted_at)?;
    let payload_hash =
        payload_hash_with_target(&notification, target_account_fingerprint.as_deref());
    let row_id = Uuid::new_v4().to_string();
    let initial_status = "pending_channel";
    let inserted = sqlx::query(
        "INSERT INTO notification_outbox (
                id, origin_kind, origin_key, origin_device_id,
                notification_id, dedupe_key, payload_hash,
                kind, target_account_fingerprint, title, body,
                correlation_key, priority, sensitive_body, status,
                not_before, expires_at, attempt_count, created_at, updated_at
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, 0, ?18, ?18
             )
             ON CONFLICT(origin_key, notification_id) DO NOTHING
             ON CONFLICT(origin_key, dedupe_key) DO NOTHING",
    )
    .bind(&row_id)
    .bind(origin.kind())
    .bind(origin.key())
    .bind(origin.device_id())
    .bind(&notification.notification_id)
    .bind(&notification.dedupe_key)
    .bind(&payload_hash)
    .bind(&notification.kind)
    .bind(&target_account_fingerprint)
    .bind(&notification.title)
    .bind(&notification.body)
    .bind(&notification.correlation_key)
    .bind(notification.priority)
    .bind(i64::from(sensitive_body))
    .bind(initial_status)
    .bind(accepted_at)
    .bind(notification.expires_at)
    .bind(accepted_at)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?
    .rows_affected();

    if inserted == 1 {
        return Ok(AcceptedNotification {
            notification_id: notification.notification_id,
            relay_status: "accepted",
            existing: false,
            accepted_at,
        });
    }

    let collisions = sqlx::query(
        "SELECT notification_id, dedupe_key, payload_hash, created_at
             FROM notification_outbox
             WHERE origin_key = ?1 AND (notification_id = ?2 OR dedupe_key = ?3)",
    )
    .bind(origin.key())
    .bind(&notification.notification_id)
    .bind(&notification.dedupe_key)
    .fetch_all(&mut **transaction)
    .await
    .map_err(database_error)?;

    if collisions.len() == 1 {
        let row = &collisions[0];
        let same_notification_id = row
            .try_get::<String, _>("notification_id")
            .map_err(database_error)?
            == notification.notification_id;
        let same_dedupe_key = row
            .try_get::<String, _>("dedupe_key")
            .map_err(database_error)?
            == notification.dedupe_key;
        let same_payload = row
            .try_get::<String, _>("payload_hash")
            .map_err(database_error)?
            == payload_hash;
        if same_notification_id && same_dedupe_key && same_payload {
            let original_accepted_at = row
                .try_get::<i64, _>("created_at")
                .map_err(database_error)?;
            return Ok(AcceptedNotification {
                notification_id: notification.notification_id,
                relay_status: "accepted",
                existing: true,
                accepted_at: original_accepted_at,
            });
        }
    }

    Err(OutboxError::IdempotencyConflict)
}

#[cfg(test)]
pub(super) fn payload_hash(notification: &RelayNotificationV1) -> String {
    payload_hash_with_target(notification, None)
}

fn payload_hash_with_target(
    notification: &RelayNotificationV1,
    target_account_fingerprint: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new_derive_key(PAYLOAD_HASH_CONTEXT);
    frame(&mut hasher, &notification.schema_version.to_be_bytes());
    frame(&mut hasher, notification.kind.as_bytes());
    frame(&mut hasher, notification.title.as_bytes());
    frame(&mut hasher, notification.body.as_bytes());
    frame(&mut hasher, &notification.priority.to_be_bytes());
    match &notification.correlation_key {
        Some(value) => {
            frame(&mut hasher, &[1]);
            frame(&mut hasher, value.as_bytes());
        }
        None => frame(&mut hasher, &[0]),
    }
    frame(&mut hasher, &notification.created_at.to_be_bytes());
    frame(&mut hasher, &notification.expires_at.to_be_bytes());
    if let Some(target) = target_account_fingerprint {
        frame(&mut hasher, b"target_account_fingerprint");
        frame(&mut hasher, target.as_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

fn frame(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

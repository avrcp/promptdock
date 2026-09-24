use promptdock_server::{
    config::{DatabaseConfig, ResultsConfig, RetentionConfig},
    db,
    outbox::BundleContentCipher,
    results::{PageState, ResultError, ResultPublicationV1, ResultService},
    retention::RetentionService,
};
use sqlx::Row as _;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[tokio::test]
async fn active_page_outlives_short_notice_retention_and_purge_preserves_replay() {
    let directory = tempfile::tempdir().unwrap();
    let pool = db::open(&DatabaseConfig {
        path: directory.path().join("retention.db"),
        ..Default::default()
    })
    .await
    .unwrap();
    let now: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    let device = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO devices(id,name,token_hash,enabled,created_at) VALUES(?1,'retention',?2,1,?3)",
    )
    .bind(device.to_string())
    .bind(vec![8_u8; 32])
    .bind(now)
    .execute(&pool)
    .await
    .unwrap();
    let mut input: ResultPublicationV1 = serde_json::from_str(include_str!(
        "../../../contracts/server-http/v1/result-publication-v1.json"
    ))
    .unwrap();
    input.created_at = now;
    input.notification_expires_at = now + 86_400_000;
    let config = ResultsConfig {
        enabled: true,
        public_origin: Some("https://relay.example.test".into()),
        max_retained_body_bytes: input.body.len(),
        ..Default::default()
    };
    let service = ResultService::new(
        pool.clone(),
        config,
        Some(BundleContentCipher::new_for_test([4; 32])),
    );
    let target = Some(format!("wx:{}", "a".repeat(64)));
    let receipt = service
        .publish(device, input.clone(), target.clone())
        .await
        .unwrap();
    let mut next = input.clone();
    next.result_id = "second-result".into();
    next.dedupe_key = "second-dedupe".into();
    assert!(matches!(
        service.publish(device, next.clone(), target.clone()).await,
        Err(ResultError::Capacity)
    ));
    sqlx::query("UPDATE notification_outbox SET status='provider_accepted',created_at=?1,updated_at=?2 WHERE notification_id=?3")
        .bind(now - 3 * 86_400_000).bind(now - 2 * 86_400_000).bind(&receipt.notification_id)
        .execute(&pool).await.unwrap();
    let retention = RetentionService::new(
        pool.clone(),
        RetentionConfig {
            accepted_days: 1,
            ..Default::default()
        },
    );
    retention.run_once(&CancellationToken::new()).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        count, 1,
        "active pages retain their frozen notice even with shorter accepted_days"
    );
    let link = service.link(device, &input.result_id).await.unwrap();
    assert_eq!(
        service
            .public_body(link.url.rsplit('/').next().unwrap())
            .await
            .unwrap()
            .0,
        input.body
    );
    sqlx::query("UPDATE results SET page_expires_at=?1,body_retain_until=?1")
        .bind(receipt.accepted_at + 1)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    retention.run_once(&CancellationToken::new()).await.unwrap();
    let row=sqlx::query("SELECT body_ciphertext,body_nonce,body_purged_at,request_digest,current_notification_id,notification_status FROM results").fetch_one(&pool).await.unwrap();
    assert!(row.get::<Option<Vec<u8>>, _>("body_ciphertext").is_none());
    assert!(row.get::<Option<Vec<u8>>, _>("body_nonce").is_none());
    assert!(row.get::<Option<i64>, _>("body_purged_at").is_some());
    assert_eq!(
        row.get::<String, _>("current_notification_id"),
        receipt.notification_id
    );
    assert_eq!(
        row.get::<String, _>("notification_status"),
        "provider_accepted"
    );
    assert_eq!(row.get::<Vec<u8>, _>("request_digest").len(), 32);
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_outbox")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let replay = service.publish(device, input, None).await.unwrap();
    assert!(matches!(replay.page_state, PageState::Expired));
    assert_eq!(replay.notification_status, "provider_accepted");
    assert_eq!(replay.notification_id, receipt.notification_id);
    assert_eq!(replay.accepted_at, receipt.accepted_at);
    service
        .publish(device, next, target)
        .await
        .expect("purged bytes release capacity");
    pool.close().await;
}

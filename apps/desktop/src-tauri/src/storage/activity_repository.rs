use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::activity::{
    ActivityAttention, ActivityCounts, ActivityDelivery, ActivityDeliveryItem, ActivityDetail,
    ActivityEventItem, ActivityFilter, ActivityItem, ActivityPage, ActivityPageRequest,
    ActivityResult, AttentionAcknowledgement, ResultSeen,
};
use crate::db::now_ms;
use crate::error::AppError;

const ATTENTION_DISPLAY_MS: i64 = 30 * 60 * 1_000;
const DETAIL_LIMIT: i64 = 100;
const JS_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

pub(crate) struct ActivityRepository;

impl ActivityRepository {
    pub(crate) fn page(
        conn: &Connection,
        request: &ActivityPageRequest,
    ) -> Result<ActivityPage, AppError> {
        Self::page_at(conn, request, now_ms()?)
    }

    pub(crate) fn detail(
        conn: &Connection,
        run_key: &str,
    ) -> Result<Option<ActivityDetail>, AppError> {
        Self::detail_at(conn, run_key, now_ms()?)
    }

    pub(crate) fn acknowledge_attention(
        tx: &Transaction<'_>,
        run_key: &str,
        observed_revision: i64,
    ) -> Result<AttentionAcknowledgement, AppError> {
        checked_revision(observed_revision)?;
        ensure_presentation(tx, run_key)?;
        let (old, current): (i64, i64) = tx.query_row(
            "SELECT acknowledged_attention_revision, attention_revision FROM run_presentation WHERE run_key = ?",
            [run_key],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).optional()?.ok_or_else(|| AppError::new("ACTIVITY_RUN_NOT_FOUND", "活动记录不存在"))?;
        checked_revision(old)?;
        checked_revision(current)?;
        let acknowledged = old.max(observed_revision.clamp(0, current));
        tx.execute(
            "UPDATE run_presentation SET acknowledged_attention_revision = ? WHERE run_key = ?",
            params![acknowledged, run_key],
        )?;
        Ok(AttentionAcknowledgement {
            acknowledged_revision: acknowledged,
            current_revision: current,
        })
    }

    pub(crate) fn mark_result_seen(
        tx: &Transaction<'_>,
        run_key: &str,
        observed_revision: i64,
    ) -> Result<ResultSeen, AppError> {
        checked_revision(observed_revision)?;
        ensure_presentation(tx, run_key)?;
        let old: i64 = tx
            .query_row(
                "SELECT seen_result_revision FROM run_presentation WHERE run_key = ?",
                [run_key],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| AppError::new("ACTIVITY_RUN_NOT_FOUND", "活动记录不存在"))?;
        let current: i64 = tx.query_row(
            "SELECT COALESCE(MAX(revision), 0) FROM (
                SELECT result_revision AS revision FROM agent_outputs WHERE run_key = ?
                UNION ALL
                SELECT COALESCE(pages.result_revision, outbox.list_result_revision)
                  FROM notification_outbox AS outbox
                  LEFT JOIN relay_result_publications AS pages ON pages.outbox_id = outbox.id
                 WHERE outbox.agent_run_key = ?
            )",
            params![run_key, run_key],
            |row| row.get(0),
        )?;
        checked_revision(old)?;
        checked_revision(current)?;
        let seen = old.max(observed_revision.clamp(0, current));
        tx.execute(
            "UPDATE run_presentation SET seen_result_revision = ? WHERE run_key = ?",
            params![seen, run_key],
        )?;
        Ok(ResultSeen {
            seen_result_revision: seen,
            current_revision: current,
        })
    }

    fn page_at(
        conn: &Connection,
        request: &ActivityPageRequest,
        now: i64,
    ) -> Result<ActivityPage, AppError> {
        let cursor = request.cursor.as_deref().map(parse_cursor).transpose()?;
        let count = i64::from(request.page_size()) + 1;
        let filter = filter_name(request.filter);
        let mut statement = conn.prepare(&format!(
            "WITH activity AS ({})
             SELECT run_key, workspace_label, display_title, activity_revision, phase, started_at,
                    last_observed_at, attention_revision, acknowledged_attention_revision,
                    attention_kind, attention_expires_at, attention_active, attention_historical, result_outbox_id,
                    result_revision, page_state, result_expires_at, seen_result_revision,
                    delivery_state, delivery_error_code, held_until, delivery_issue
             FROM activity
             WHERE (?2 = 'all' OR (?2 = 'attention' AND attention_active = 1)
                 OR (?2 = 'started' AND phase = 'started')
                 OR (?2 = 'results' AND (result_outbox_id IS NOT NULL OR phase = 'ended_observed'))
                 OR (?2 = 'delivery_issues' AND delivery_issue = 1)
                 OR (?2 = 'recent' AND last_observed_at >= ?3))
               AND (?4 IS NULL OR last_observed_at < ?4 OR (last_observed_at = ?4 AND run_key < ?5))
             ORDER BY last_observed_at DESC, run_key DESC LIMIT ?6",
            activity_cte()
        ))?;
        let recent_cutoff = now.saturating_sub(24 * 60 * 60 * 1_000);
        let rows = statement
            .query_map(
                params![
                    now,
                    filter,
                    recent_cutoff,
                    cursor.as_ref().map(|value| value.0),
                    cursor.as_ref().map(|value| value.1.as_str()),
                    count
                ],
                read_item,
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut items = rows;
        let next_cursor = if items.len() > usize::from(request.page_size()) {
            items.pop();
            let last = items
                .last()
                .expect("non-empty page after removing lookahead");
            Some(encode_cursor(last.last_observed_at, &last.run_key)?)
        } else {
            None
        };
        Ok(ActivityPage {
            items,
            next_cursor,
            counts: Self::counts_at(conn, now)?,
        })
    }

    fn counts_at(conn: &Connection, now: i64) -> Result<ActivityCounts, AppError> {
        let recent_cutoff = now.saturating_sub(24 * 60 * 60 * 1_000);
        conn.query_row(
            &format!(
                "WITH activity AS ({}) SELECT
              COALESCE(SUM(attention_active), 0),
              COALESCE(SUM(phase = 'started'), 0),
              COALESCE(SUM(result_outbox_id IS NOT NULL OR phase = 'ended_observed'), 0),
              COALESCE(SUM(delivery_issue), 0),
              COALESCE(SUM(last_observed_at >= ?), 0)
             FROM activity",
                activity_cte()
            ),
            params![now, recent_cutoff],
            |row| {
                Ok(ActivityCounts {
                    attention: to_count(row.get(0)?)?,
                    started: to_count(row.get(1)?)?,
                    results: to_count(row.get(2)?)?,
                    delivery_issues: to_count(row.get(3)?)?,
                    recent: to_count(row.get(4)?)?,
                })
            },
        )
        .map_err(AppError::from)
    }

    fn detail_at(
        conn: &Connection,
        run_key: &str,
        now: i64,
    ) -> Result<Option<ActivityDetail>, AppError> {
        let item = conn.query_row(&format!(
            "WITH activity AS ({}) SELECT run_key, workspace_label, display_title, activity_revision, phase, started_at,
                    last_observed_at, attention_revision, acknowledged_attention_revision,
                    attention_kind, attention_expires_at, attention_active, attention_historical, result_outbox_id,
                    result_revision, page_state, result_expires_at, seen_result_revision,
                    delivery_state, delivery_error_code, held_until, delivery_issue
             FROM activity WHERE run_key = ?", activity_cte()
        ), params![now, run_key], read_item).optional()?;
        let Some(item) = item else {
            return Ok(None);
        };
        let mut events = conn.prepare(
            "SELECT id, event_kind, occurred_at, observed_at FROM agent_events WHERE run_key = ?
             ORDER BY occurred_at DESC, id DESC LIMIT ?",
        )?.query_map(params![run_key, DETAIL_LIMIT], |row| Ok(ActivityEventItem {
            id: row.get(0)?, kind: row.get(1)?, occurred_at: row.get(2)?, observed_at: row.get(3)?,
        }))?.collect::<Result<Vec<_>, _>>()?;
        let deliveries = conn.prepare(
            "SELECT outbox.id, outbox.event_kind, outbox.status, pages.page_state, COALESCE(pages.result_revision, outbox.list_result_revision)
             FROM notification_outbox AS outbox
             LEFT JOIN relay_result_publications AS pages ON pages.outbox_id = outbox.id
             WHERE outbox.agent_run_key = ?
             ORDER BY outbox.created_at DESC, outbox.id DESC LIMIT ?",
        )?.query_map(params![run_key, DETAIL_LIMIT], |row| Ok(ActivityDeliveryItem {
            id: row.get(0)?, kind: row.get(1)?, state: row.get(2)?, page_state: row.get(3)?, result_revision: row.get(4)?,
        }))?.collect::<Result<Vec<_>, _>>()?;
        events.shrink_to_fit();
        Ok(Some(ActivityDetail {
            item,
            events,
            deliveries,
        }))
    }
}

fn activity_cte() -> String {
    format!(
        "WITH ranked_results AS (
             SELECT outbox.agent_run_key, outbox.id AS outbox_id, outbox.status, outbox.last_error_code, outbox.remote_status, outbox.acceptance_stage,
                    COALESCE(pages.result_revision, outbox.list_result_revision) AS result_revision,
                    pages.page_state, pages.page_expires_at,
                    ROW_NUMBER() OVER (PARTITION BY outbox.agent_run_key ORDER BY COALESCE(pages.result_revision, outbox.list_result_revision) DESC, outbox.id DESC) AS rank
               FROM notification_outbox AS outbox
               LEFT JOIN relay_result_publications AS pages ON pages.outbox_id = outbox.id
              WHERE outbox.agent_run_key IS NOT NULL AND COALESCE(pages.result_revision, outbox.list_result_revision) >= 1
           ), latest_notices AS (
             SELECT agent_run_key, status, last_error_code, remote_status, acceptance_stage,
                    ROW_NUMBER() OVER (PARTITION BY agent_run_key ORDER BY created_at DESC, id DESC) AS rank
               FROM notification_outbox WHERE agent_run_key IS NOT NULL
           ), delivery_issues AS (
             SELECT agent_run_key, 1 AS has_issue FROM notification_outbox
              WHERE status IN ('retry_wait', 'blocked_activation', 'blocked_reconnect', 'expired', 'dead_letter')
              GROUP BY agent_run_key
           )
           SELECT runs.run_key, COALESCE(presentation.workspace_label, '') AS workspace_label, 'Codex 编码轮次·' || substr(runs.run_key, 1, 12) AS display_title,
                COALESCE(presentation.activity_revision, 0) AS activity_revision,
                CASE runs.status WHEN 'running' THEN 'started' WHEN 'settling' THEN 'settling'
                  WHEN 'completed' THEN 'ended_observed' WHEN 'interrupted' THEN 'interrupted'
                  WHEN 'failed' THEN 'failed_observed' WHEN 'cancelled' THEN 'cancelled' ELSE 'unknown' END AS phase,
                runs.started_at, MAX(runs.last_event_at, COALESCE(presentation.updated_at, 0)) AS last_observed_at,
                COALESCE(presentation.attention_revision, 0) AS attention_revision, COALESCE(presentation.acknowledged_attention_revision, 0) AS acknowledged_attention_revision,
                CASE attention.safe_summary
                  WHEN 'Shell / command execution' THEN '命令执行'
                  WHEN 'File modification' THEN '文件修改'
                  WHEN 'File access' THEN '文件访问'
                  WHEN 'Network access' THEN '网络访问'
                  ELSE '权限请求' END AS attention_kind,
                CASE WHEN presentation.last_attention_at IS NULL THEN NULL ELSE COALESCE(attention.expires_at, presentation.last_attention_at + {ATTENTION_DISPLAY_MS}) END AS attention_expires_at,
                CASE WHEN runs.status = 'running' AND COALESCE(presentation.attention_revision, 0) > COALESCE(presentation.acknowledged_attention_revision, 0)
                    AND presentation.last_attention_at IS NOT NULL
                    AND presentation.last_attention_at >= ?1 - {ATTENTION_DISPLAY_MS}
                    AND COALESCE(attention.expires_at, presentation.last_attention_at + {ATTENTION_DISPLAY_MS}) > ?1
                  THEN 1 ELSE 0 END AS attention_active,
                CASE WHEN presentation.last_attention_at IS NOT NULL
                           AND (runs.status <> 'running' OR COALESCE(attention.expires_at, presentation.last_attention_at + {ATTENTION_DISPLAY_MS}) <= ?1)
                  THEN 1 ELSE 0 END AS attention_historical,
                result.outbox_id AS result_outbox_id, COALESCE(result.result_revision, 0) AS result_revision,
                CASE WHEN result.page_state = 'available' AND result.page_expires_at <= ?1 THEN 'expired' ELSE result.page_state END AS page_state,
                result.page_expires_at AS result_expires_at,
                COALESCE(presentation.seen_result_revision, 0) AS seen_result_revision,
                COALESCE(result.remote_status, CASE result.acceptance_stage WHEN 'relay' THEN 'relay_accepted' WHEN 'provider' THEN 'provider_accepted' ELSE result.status END,
                         notice.remote_status, CASE notice.acceptance_stage WHEN 'relay' THEN 'relay_accepted' WHEN 'provider' THEN 'provider_accepted' ELSE notice.status END) AS delivery_state,
                COALESCE(result.last_error_code, notice.last_error_code) AS delivery_error_code,
                NULL AS held_until, COALESCE(issues.has_issue, 0) AS delivery_issue
         FROM agent_runs AS runs
         LEFT JOIN run_presentation AS presentation ON presentation.run_key = runs.run_key
         LEFT JOIN attention_events AS attention ON attention.id = presentation.last_attention_event_id
         LEFT JOIN ranked_results AS result ON result.agent_run_key = runs.run_key AND result.rank = 1
         LEFT JOIN latest_notices AS notice ON notice.agent_run_key = runs.run_key AND notice.rank = 1
         LEFT JOIN delivery_issues AS issues ON issues.agent_run_key = runs.run_key
         WHERE runs.parent_run_key IS NULL"
    )
}

fn read_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityItem> {
    let attention_revision: i64 = row.get(7)?;
    let attention_acknowledged_revision: i64 = row.get(8)?;
    let attention_label: Option<String> = row.get(9)?;
    let attention_expires_at: Option<i64> = row.get(10)?;
    let attention_historical: i64 = row.get(12)?;
    let result_outbox_id: Option<String> = row.get(13)?;
    let result_revision: i64 = row.get(14)?;
    let page_state: Option<String> = row.get(15)?;
    let result_expires_at: Option<i64> = row.get(16)?;
    let seen_result_revision: i64 = row.get(17)?;
    let delivery_state: Option<String> = row.get(18)?;
    let delivery_error_code: Option<String> = row.get(19)?;
    let held_until: Option<i64> = row.get(20)?;
    Ok(ActivityItem {
        run_key: row.get(0)?,
        workspace_label: row.get(1)?,
        display_title: row.get(2)?,
        activity_revision: row.get(3)?,
        phase: row.get(4)?,
        started_at: row.get(5)?,
        last_observed_at: row.get(6)?,
        attention: (attention_revision > 0).then(|| ActivityAttention {
            revision: attention_revision,
            acknowledged_revision: attention_acknowledged_revision,
            label: attention_label.unwrap_or_else(|| "attention".into()),
            historical: attention_historical != 0,
            observation_expires_at: attention_expires_at,
        }),
        result: result_outbox_id.map(|outbox_id| ActivityResult {
            outbox_id,
            result_revision,
            page_state,
            expires_at: result_expires_at,
            seen_result_revision,
        }),
        delivery: delivery_state.map(|state| ActivityDelivery {
            state,
            last_error_code: delivery_error_code,
            held_until,
        }),
    })
}

fn filter_name(value: ActivityFilter) -> &'static str {
    match value {
        ActivityFilter::All => "all",
        ActivityFilter::Attention => "attention",
        ActivityFilter::Started => "started",
        ActivityFilter::Results => "results",
        ActivityFilter::DeliveryIssues => "delivery_issues",
        ActivityFilter::Recent => "recent",
    }
}
fn to_count(value: i64) -> Result<u32, rusqlite::Error> {
    u32::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, value))
}
fn checked_revision(value: i64) -> Result<(), AppError> {
    if (0..=JS_SAFE_INTEGER).contains(&value) {
        Ok(())
    } else {
        Err(AppError::new(
            "ACTIVITY_REVISION_INVALID",
            "活动版本超出 JavaScript 安全整数范围",
        ))
    }
}
fn ensure_presentation(tx: &Transaction<'_>, run_key: &str) -> Result<(), AppError> {
    tx.execute(
        "INSERT INTO run_presentation(run_key, updated_at)
         SELECT run_key, last_event_at FROM agent_runs WHERE run_key = ?
         ON CONFLICT(run_key) DO NOTHING",
        params![run_key],
    )?;
    Ok(())
}
fn encode_cursor(last_observed_at: i64, run_key: &str) -> Result<String, AppError> {
    serde_json::to_string(&(last_observed_at, run_key))
        .map_err(|_| AppError::new("ACTIVITY_CURSOR_INVALID", "活动分页游标无效"))
}
fn parse_cursor(value: &str) -> Result<(i64, String), AppError> {
    let (time, key): (i64, String) = serde_json::from_str(value)
        .map_err(|_| AppError::new("ACTIVITY_CURSOR_INVALID", "活动分页游标无效"))?;
    if time < 0 || key.is_empty() || key.len() > 256 {
        return Err(AppError::new("ACTIVITY_CURSOR_INVALID", "活动分页游标无效"));
    }
    Ok((time, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::{ActivityFilter, ActivityPageRequest};
    use crate::db::Db;
    use std::collections::BTreeSet;
    use std::time::Instant;

    const NOW: i64 = 2_000_000;

    fn setup() -> Db {
        let db = Db::open_in_memory().unwrap();
        db.with_transaction(|tx| {
            tx.execute("INSERT INTO source_streams(source_key,agent_kind,source_id,source_kind,instance_id,created_at) VALUES('source','codex','fixture','hook','default',1)", [])?;
            Ok(())
        }).unwrap();
        db
    }

    fn seed_run(db: &Db, key: &str, observed_at: i64) {
        db.with_transaction(|tx| {
            tx.execute("INSERT INTO agent_runs(run_key,agent_kind,instance_id,agent_label,status,outcome,completion_confidence,last_event_at) VALUES(?, 'codex', 'default', 'Fixture', 'running', 'unknown', 'provisional', ?)", params![key, observed_at])?;
            tx.execute("INSERT INTO run_presentation(run_key,updated_at) VALUES(?, ?)", params![key, observed_at])?;
            Ok(())
        }).unwrap();
    }

    fn seed_output(db: &Db, run_key: &str, id: &str, revision: i64, raw_text: &str) {
        db.with_transaction(|tx| {
            tx.execute("INSERT INTO agent_outputs(id,run_key,source_key,event_id,output_kind,raw_text,content_hash,content_mode,content_available,content_bytes,result_revision,is_final,occurred_at,observed_at) VALUES(?, ?, 'source', ?, 'assistant_final', ?, 'hash', 'full_final', 1, ?, ?, 1, 1, 1)", params![id, run_key, id, raw_text, raw_text.len(), revision])?;
            Ok(())
        }).unwrap();
    }

    fn seed_result_outbox(db: &Db, run_key: &str, id: &str, revision: i64, publication: bool) {
        db.with_transaction(|tx| {
            tx.execute("INSERT INTO notification_outbox(id,agent_run_key,event_kind,dedupe_key,client_id,payload_json,priority,status,not_before,expires_at,created_at,updated_at,list_result_revision) VALUES(?,?,'run_completed',?,?,'{}',1,'pending',1,9999999,1,1,?)", params![id, run_key, format!("dedupe-{id}"), format!("client-{id}"), revision])?;
            if publication {
                tx.execute("INSERT INTO relay_result_publications(outbox_id,source_hash,result_revision,destination_identity,request_digest) VALUES(?, ?, ?, 'fixture', ?)", params![id, "a".repeat(64), revision, "b".repeat(64)])?;
            }
            Ok(())
        }).unwrap();
    }

    #[test]
    fn runs_without_a_presentation_projection_remain_visible() {
        let db = setup();
        db.with_transaction(|tx| {
            tx.execute("INSERT INTO agent_runs(run_key,agent_kind,instance_id,agent_label,status,outcome,completion_confidence,last_event_at) VALUES('unprojected', 'codex', 'default', 'Fixture', 'running', 'unknown', 'provisional', ?)", [NOW])?;
            Ok(())
        }).unwrap();
        let page = db
            .with_connection(|conn| {
                ActivityRepository::page_at(conn, &ActivityPageRequest::default(), NOW)
            })
            .unwrap();
        assert_eq!(page.items[0].run_key, "unprojected");
        assert_eq!(page.items[0].activity_revision, 0);
    }

    #[test]
    fn activity_aggregate_sql_prepares_against_the_v7_schema() {
        let db = setup();
        db.with_connection(|conn| {
            conn.prepare(&format!(
                "WITH activity AS ({}) SELECT * FROM activity",
                activity_cte()
            ))
            .unwrap();
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn page_is_keyset_paginated_and_never_reads_result_bodies() {
        let db = setup();
        let sentinel =
            "TOP_SECRET_RESULT_BODY".repeat(256 * 1024 / "TOP_SECRET_RESULT_BODY".len() + 1);
        for index in 0..100 {
            let key = format!("run-{index:03}");
            seed_run(&db, &key, NOW - i64::from(index));
            if index < 20 {
                seed_output(
                    &db,
                    &key,
                    &format!("output-{index:03}"),
                    1,
                    &sentinel[..256 * 1024],
                );
                seed_result_outbox(&db, &key, &format!("outbox-{index:03}"), 1, true);
            }
        }
        let mut cursor: Option<String> = None;
        let mut keys = BTreeSet::new();
        let mut samples = Vec::new();
        for _ in 0..40 {
            let started = Instant::now();
            let page = db
                .with_connection(|conn| {
                    ActivityRepository::page_at(
                        conn,
                        &ActivityPageRequest {
                            cursor: cursor.clone(),
                            ..Default::default()
                        },
                        NOW,
                    )
                })
                .unwrap();
            samples.push(started.elapsed());
            assert!(!serde_json::to_string(&page)
                .unwrap()
                .contains("TOP_SECRET_RESULT_BODY"));
            cursor = page.next_cursor;
            if cursor.is_none() {
                cursor = None;
            }
        }
        loop {
            let page = db
                .with_connection(|conn| {
                    ActivityRepository::page_at(
                        conn,
                        &ActivityPageRequest {
                            cursor: cursor.clone(),
                            ..Default::default()
                        },
                        NOW,
                    )
                })
                .unwrap();
            keys.extend(page.items.into_iter().map(|item| item.run_key));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        samples.sort();
        let p95 = samples[(samples.len() * 95 / 100).min(samples.len() - 1)];
        eprintln!(
            "activity metadata page p95={}ms (40 samples)",
            p95.as_millis()
        );
        assert_eq!(
            keys.len(),
            100,
            "keyset must have neither omissions nor duplicates"
        );
        assert!(
            p95.as_millis() <= 200,
            "metadata-only query p95 exceeded 200ms"
        );
    }

    #[test]
    fn attention_filter_uses_expiry_and_root_runs_only() {
        let db = setup();
        seed_run(&db, "root", NOW);
        seed_run(&db, "expired", NOW - 1);
        db.with_transaction(|tx| {
            tx.execute("UPDATE run_presentation SET attention_revision=1,last_attention_at=? WHERE run_key='root'", [NOW - 1])?;
            tx.execute("UPDATE run_presentation SET attention_revision=1,last_attention_at=? WHERE run_key='expired'", [NOW - 1])?;
            tx.execute("UPDATE agent_runs SET parent_run_key='root' WHERE run_key='expired'", [])?;
            Ok(())
        }).unwrap();
        let page = db
            .with_connection(|conn| {
                ActivityRepository::page_at(
                    conn,
                    &ActivityPageRequest {
                        filter: ActivityFilter::Attention,
                        ..Default::default()
                    },
                    NOW,
                )
            })
            .unwrap();
        assert_eq!(
            page.items
                .iter()
                .map(|item| item.run_key.as_str())
                .collect::<Vec<_>>(),
            vec!["root"]
        );
        assert!(!page.items[0].attention.as_ref().unwrap().historical);
    }

    #[test]
    fn watermarks_clamp_to_current_and_cannot_move_backwards() {
        let db = setup();
        seed_run(&db, "run", NOW);
        seed_output(&db, "run", "output-1", 1, "private");
        seed_output(&db, "run", "output-2", 2, "newer private");
        db.with_transaction(|tx| {
            tx.execute("UPDATE run_presentation SET attention_revision=5, acknowledged_attention_revision=2 WHERE run_key='run'", [])?;
            let ack = ActivityRepository::acknowledge_attention(tx, "run", 99)?;
            assert_eq!(ack.acknowledged_revision, 5);
            let ack = ActivityRepository::acknowledge_attention(tx, "run", 1)?;
            assert_eq!(ack.acknowledged_revision, 5);
            let seen = ActivityRepository::mark_result_seen(tx, "run", 99)?;
            assert_eq!((seen.seen_result_revision, seen.current_revision), (2, 2));
            let seen = ActivityRepository::mark_result_seen(tx, "run", 1)?;
            assert_eq!(seen.seen_result_revision, 2);
            Ok(())
        }).unwrap();
    }

    #[test]
    fn acknowledgement_races_keep_new_attention_unread_without_changing_run_facts() {
        for acknowledge_first in [false, true] {
            let db = setup();
            seed_run(&db, "run", NOW);
            db.with_transaction(|tx| {
                tx.execute("UPDATE run_presentation SET attention_revision=1,last_attention_at=? WHERE run_key='run'", [NOW - 1])?;
                if acknowledge_first { ActivityRepository::acknowledge_attention(tx, "run", 1)?; }
                tx.execute("UPDATE run_presentation SET attention_revision=2,last_attention_at=? WHERE run_key='run'", [NOW])?;
                if !acknowledge_first { ActivityRepository::acknowledge_attention(tx, "run", 1)?; }
                Ok(())
            }).unwrap();
            let page = db
                .with_connection(|conn| {
                    ActivityRepository::page_at(conn, &ActivityPageRequest::default(), NOW)
                })
                .unwrap();
            assert_eq!(page.counts.attention, 1);
            assert!(page.items[0]
                .attention
                .as_ref()
                .is_some_and(|attention| attention.revision > attention.acknowledged_revision));
            let facts = db
                .with_connection(|conn| {
                    conn.query_row(
                        "SELECT status,outcome FROM agent_runs WHERE run_key='run'",
                        [],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .map_err(AppError::from)
                })
                .unwrap();
            assert_eq!(facts, ("running".into(), "unknown".into()));
        }
    }

    #[test]
    fn terminal_and_late_attention_are_historical_and_expired_observation_is_not_counted() {
        let db = setup();
        seed_run(&db, "settling", NOW);
        seed_run(&db, "late", NOW);
        db.with_transaction(|tx| {
            tx.execute("UPDATE agent_runs SET status='settling' WHERE run_key='settling'", [])?;
            tx.execute("UPDATE run_presentation SET attention_revision=1,last_attention_at=? WHERE run_key='settling'", [NOW])?;
            tx.execute("INSERT INTO agent_events(id,source_key,event_id,event_kind,run_key,occurred_at,observed_at,metadata_json,payload_hash) VALUES('late-attention','source','late-attention','attention_required','late',10,10,'{}','hash')", [])?;
            tx.execute("INSERT INTO attention_events(id,run_key,kind,safe_summary,occurred_at,expires_at) VALUES('late-attention','late','permission','File access',10,?)", [10 + ATTENTION_DISPLAY_MS])?;
            tx.execute("UPDATE run_presentation SET attention_revision=1,last_attention_event_id='late-attention',last_attention_at=? WHERE run_key='late'", [NOW])?;
            Ok(())
        }).unwrap();
        let page = db
            .with_connection(|conn| {
                ActivityRepository::page_at(conn, &ActivityPageRequest::default(), NOW)
            })
            .unwrap();
        assert_eq!(page.counts.attention, 0);
        assert!(page
            .items
            .iter()
            .filter_map(|item| item.attention.as_ref())
            .all(|attention| attention.historical));
    }

    #[test]
    fn newest_result_wins_over_old_publication_replay_and_pending_outbox_is_visible() {
        let db = setup();
        seed_run(&db, "run", NOW);
        seed_result_outbox(&db, "run", "old", 1, true);
        seed_result_outbox(&db, "run", "new", 2, false);
        db.with_transaction(|tx| {
            tx.execute(
                "UPDATE notification_outbox SET updated_at=999999 WHERE id='old'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let item = db
            .with_connection(|conn| {
                ActivityRepository::page_at(conn, &ActivityPageRequest::default(), NOW)
            })
            .unwrap()
            .items
            .remove(0);
        let result = item.result.unwrap();
        assert_eq!(
            (result.outbox_id, result.result_revision, result.page_state),
            ("new".into(), 2, None)
        );
    }

    #[test]
    fn watermark_inputs_must_be_javascript_safe_integers() {
        assert_eq!(
            checked_revision(-1).unwrap_err().code,
            "ACTIVITY_REVISION_INVALID"
        );
        assert_eq!(
            checked_revision(JS_SAFE_INTEGER + 1).unwrap_err().code,
            "ACTIVITY_REVISION_INVALID"
        );
    }

    #[test]
    fn reading_old_run_never_reorders_it_and_expired_page_is_not_available() {
        let db = setup();
        seed_run(&db, "old", 10);
        seed_result_outbox(&db, "old", "result", 1, true);
        db.with_transaction(|tx| {
            tx.execute("UPDATE relay_result_publications SET page_state='available',page_expires_at=1000,accepted_at=10,notification_id='notice',notification_status='provider_accepted',updated_at=10 WHERE outbox_id='result'",[])?;
            tx.execute("DELETE FROM run_presentation WHERE run_key='old'", [])?;
            ActivityRepository::acknowledge_attention(tx, "old", 0)?;
            Ok(())
        })
        .unwrap();
        let item = db
            .with_connection(|conn| {
                ActivityRepository::page_at(conn, &ActivityPageRequest::default(), 10_000_000)
            })
            .unwrap()
            .items
            .remove(0);
        assert_eq!(item.last_observed_at, 10);
        assert_eq!(item.result.unwrap().page_state.as_deref(), Some("expired"));
    }

    #[test]
    fn presentation_foreign_key_rejects_orphan_rows() {
        let db = setup();
        let error = db
            .with_transaction(|tx| {
                tx.execute(
                    "INSERT INTO run_presentation(run_key,updated_at) VALUES('missing', 1)",
                    [],
                )?;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error.code, "STORE_UNAVAILABLE");
    }
}

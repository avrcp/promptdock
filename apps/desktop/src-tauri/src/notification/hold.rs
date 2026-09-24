use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::Poll;

use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::db::Db;
use crate::error::AppError;

pub(crate) const HOLD_METADATA_KEY: &str = "notification_user_hold_v1";
const HOLD_15_MINUTES_MS: i64 = 15 * 60 * 1_000;
const HOLD_60_MINUTES_MS: i64 = 60 * 60 * 1_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum HoldDuration {
    Minutes15,
    Minutes60,
}

impl HoldDuration {
    pub(crate) const fn milliseconds(self) -> i64 {
        match self {
            Self::Minutes15 => HOLD_15_MINUTES_MS,
            Self::Minutes60 => HOLD_60_MINUTES_MS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UserHoldState {
    Active,
    Inactive,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserHoldStatus {
    pub revision: u64,
    pub started_at: Option<i64>,
    pub until: Option<i64>,
    pub requested_duration: Option<i64>,
    pub state: UserHoldState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct HoldReceipt {
    pub status: &'static str,
    pub state: UserHoldStatus,
}

/// An intentionally opaque, single-request authorization. It has no durable
/// capability outside the transaction which created it: callers must acquire
/// it immediately before beginning their outbound request.
pub(crate) struct SendPermit {
    controller: HoldController,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpStartError {
    Held,
    Cancelled,
}

/// Marks the exact linearization point at which an HTTP request starts. The
/// guard is held through the request only as accounting; it never holds the
/// gate mutex or a SQLite connection.
pub(crate) struct HttpStartGuard {
    gate: Arc<Mutex<HoldGate>>,
}

impl Drop for HttpStartGuard {
    fn drop(&mut self) {
        if let Ok(mut gate) = self.gate.lock() {
            gate.in_flight = gate.in_flight.saturating_sub(1);
        }
    }
}

#[derive(Default)]
struct HoldGate {
    generation: u64,
    release_generation: u64,
    held: bool,
    in_flight: u32,
}

impl SendPermit {
    /// Starts the supplied HTTP future at a linearization point shared with a
    /// hold commit. The first poll runs while the short gate is held; the gate
    /// is released before a pending future can await network I/O.
    pub(crate) async fn start_http<F, C>(
        self,
        future: F,
        is_cancelled: C,
    ) -> Result<F::Output, HttpStartError>
    where
        F: std::future::Future,
        C: Fn() -> bool,
    {
        let mut future = std::pin::pin!(future);
        let mut started: Option<HttpStartGuard> = None;
        std::future::poll_fn(|context| {
            if started.is_none() {
                let mut gate = match self.controller.gate.lock() {
                    Ok(gate) => gate,
                    Err(_) => return Poll::Ready(Err(HttpStartError::Held)),
                };
                if gate.held || gate.generation != self.generation {
                    return Poll::Ready(Err(HttpStartError::Held));
                }
                let allowed = self
                    .controller
                    .db
                    .with_transaction(|tx| {
                        self.controller
                            .permits_send_tx(tx, crate::db::now_ms().unwrap_or(0))
                    })
                    .unwrap_or(false);
                if !allowed {
                    self.controller.sync_gate_state(&mut gate, true);
                    return Poll::Ready(Err(HttpStartError::Held));
                }
                // Hold has priority when both signals become ready before the
                // first poll. With an allowed gate, cancellation wins without
                // starting an otherwise unnecessary request.
                if is_cancelled() {
                    return Poll::Ready(Err(HttpStartError::Cancelled));
                }
                gate.in_flight = gate.in_flight.saturating_add(1);
                started = Some(HttpStartGuard {
                    gate: Arc::clone(&self.controller.gate),
                });
                // The first poll is intentionally inside the gate. Do not
                // retain it across Pending: only request start is serialized.
                let result = future.as_mut().poll(context);
                drop(gate);
                return match result {
                    Poll::Ready(result) => {
                        started.take();
                        Poll::Ready(Ok(result))
                    }
                    Poll::Pending => Poll::Pending,
                };
            }
            match future.as_mut().poll(context) {
                Poll::Ready(result) => {
                    started.take();
                    Poll::Ready(Ok(result))
                }
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct HoldDocument {
    revision: u64,
    started_at: Option<i64>,
    until: Option<i64>,
    requested_duration: Option<i64>,
}

/// The hold document is deliberately independent from capture policy.  It is
/// consulted only in the transaction that turns an outbox row into `sending`.
#[derive(Clone)]
pub(crate) struct HoldController {
    db: Arc<Db>,
    gate: Arc<Mutex<HoldGate>>,
}

impl HoldController {
    pub(crate) fn new(db: Arc<Db>) -> Self {
        Self {
            gate: gate_for_db(&db),
            db,
        }
    }

    pub(crate) fn status_at(&self, now: i64) -> Result<UserHoldStatus, AppError> {
        self.db.with_connection(|conn| load_status(conn, now))
    }

    /// Used by the manual test path before it enqueues work. It has no rate
    /// limiter and does not grant a send authorization by itself.
    pub(crate) fn require_not_held(&self, now: i64) -> Result<(), AppError> {
        match self.status_at(now)?.state {
            UserHoldState::Inactive => Ok(()),
            UserHoldState::Active => {
                Err(AppError::new("NOTIFICATION_HOLD_ACTIVE", "通知投递暂缓中"))
            }
            UserHoldState::Uncertain => Err(AppError::new(
                "NOTIFICATION_HOLD_UNCERTAIN",
                "通知暂缓状态需要明确恢复",
            )),
        }
    }

    /// Used by a manual resend immediately after its operation lock. Automatic
    /// workers use `permits_send_tx` in their claim transaction instead.
    pub(crate) fn acquire_send_permit(&self, now: i64) -> Result<Option<SendPermit>, AppError> {
        let mut gate = self.gate()?;
        let allowed = self
            .db
            .with_transaction(|tx| self.permits_send_tx(tx, now))?;
        self.sync_gate_state(&mut gate, !allowed);
        Ok(allowed.then_some(SendPermit {
            controller: self.clone(),
            generation: gate.generation,
        }))
    }

    pub(crate) fn hold(
        &self,
        expected_revision: u64,
        duration: HoldDuration,
        now: i64,
    ) -> Result<HoldReceipt, AppError> {
        valid_now(now)?;
        let mut gate = self.gate()?;
        let receipt = self.db.with_transaction(|tx| {
            let current = load_document_tx(tx)?;
            let (revision, valid) = match current {
                Some(LoadedDocument::Valid(document)) => (document.revision, true),
                Some(LoadedDocument::Invalid) => (0, false),
                None => (0, true),
            };
            if !valid || revision != expected_revision {
                return Ok(HoldReceipt {
                    status: "conflict",
                    state: status_from_loaded(load_document_tx(tx)?, now),
                });
            }
            let revision = next_revision(revision)?;
            let requested_duration = duration.milliseconds();
            let until = now
                .checked_add(requested_duration)
                .ok_or_else(|| AppError::new("INVALID_HOLD_TIME", "通知暂缓时间无效"))?;
            let document = HoldDocument {
                revision,
                started_at: Some(now),
                until: Some(until),
                requested_duration: Some(requested_duration),
            };
            store_tx(tx, &document)?;
            Ok(HoldReceipt {
                status: "applied",
                state: status_from_document(&document, now),
            })
        })?;
        if receipt.status == "applied" {
            self.sync_gate_state(&mut gate, true);
        }
        Ok(receipt)
    }

    /// Explicit resume is the sole recovery path for a corrupt or clock-rolled
    /// hold record. Invalid records deliberately have revision zero.
    pub(crate) fn resume(&self, expected_revision: u64, now: i64) -> Result<HoldReceipt, AppError> {
        valid_now(now)?;
        let mut gate = self.gate()?;
        let receipt = self.db.with_transaction(|tx| {
            let current = load_document_tx(tx)?;
            let revision = match current.as_ref() {
                Some(LoadedDocument::Valid(document)) => document.revision,
                Some(LoadedDocument::Invalid) | None => 0,
            };
            if revision != expected_revision {
                return Ok(HoldReceipt {
                    status: "conflict",
                    state: status_from_loaded(current, now),
                });
            }
            let document = HoldDocument {
                revision: next_revision(revision)?,
                started_at: None,
                until: None,
                requested_duration: None,
            };
            store_tx(tx, &document)?;
            Ok(HoldReceipt {
                status: "applied",
                state: status_from_document(&document, now),
            })
        })?;
        if receipt.status == "applied" {
            self.sync_gate_state(&mut gate, false);
        }
        Ok(receipt)
    }

    /// Called inside the outbox claim transaction. Returning false means no
    /// row may be claimed and therefore no attempt counter is consumed.
    pub(crate) fn permits_send_tx(&self, tx: &Transaction<'_>, now: i64) -> Result<bool, AppError> {
        valid_now(now)?;
        match load_document_tx(tx)? {
            None => Ok(true),
            Some(LoadedDocument::Valid(document)) => {
                match status_from_document(&document, now).state {
                    UserHoldState::Inactive => Ok(true),
                    UserHoldState::Active | UserHoldState::Uncertain => Ok(false),
                }
            }
            Some(LoadedDocument::Invalid) => Ok(false),
        }
    }

    pub(crate) fn release_generation(&self) -> u64 {
        self.gate
            .lock()
            .map(|gate| gate.release_generation)
            .unwrap_or(0)
    }

    fn gate(&self) -> Result<std::sync::MutexGuard<'_, HoldGate>, AppError> {
        self.gate
            .lock()
            .map_err(|_| AppError::new("NOTIFICATION_HOLD_UNAVAILABLE", "通知暂缓状态不可用"))
    }

    fn sync_gate_state(&self, gate: &mut HoldGate, held: bool) {
        if gate.held != held {
            gate.held = held;
            gate.generation = gate.generation.wrapping_add(1);
            if !held {
                gate.release_generation = gate.release_generation.wrapping_add(1);
            }
        }
    }
}

fn gate_for_db(db: &Arc<Db>) -> Arc<Mutex<HoldGate>> {
    static GATES: OnceLock<Mutex<HashMap<usize, Weak<Mutex<HoldGate>>>>> = OnceLock::new();
    let key = Arc::as_ptr(db) as usize;
    let mut gates = GATES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = Arc::new(Mutex::new(HoldGate::default()));
    gates.insert(key, Arc::downgrade(&gate));
    gate
}

enum LoadedDocument {
    Valid(HoldDocument),
    Invalid,
}

fn valid_now(now: i64) -> Result<(), AppError> {
    if now < 0 {
        return Err(AppError::new("INVALID_HOLD_TIME", "通知暂缓时间无效"));
    }
    Ok(())
}

fn load_status(conn: &rusqlite::Connection, now: i64) -> Result<UserHoldStatus, AppError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT substr(value, 1, 257) FROM app_metadata WHERE key = ?",
            [HOLD_METADATA_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(AppError::from)?;
    Ok(status_from_loaded(raw.map(parse_document), now))
}

fn load_document_tx(tx: &Transaction<'_>) -> Result<Option<LoadedDocument>, AppError> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT substr(value, 1, 257) FROM app_metadata WHERE key = ?",
            [HOLD_METADATA_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(AppError::from)?;
    Ok(raw.map(parse_document))
}

fn parse_document(raw: String) -> LoadedDocument {
    if raw.len() > 256 {
        return LoadedDocument::Invalid;
    }
    match serde_json::from_str::<HoldDocument>(&raw) {
        Ok(document) if valid_document(&document) => LoadedDocument::Valid(document),
        Ok(_) | Err(_) => LoadedDocument::Invalid,
    }
}

fn valid_document(document: &HoldDocument) -> bool {
    if document.revision > MAX_SAFE_INTEGER {
        return false;
    }
    match (
        document.started_at,
        document.until,
        document.requested_duration,
    ) {
        (None, None, None) => true,
        (Some(started_at), Some(until), Some(duration)) => {
            started_at >= 0
                && matches!(duration, HOLD_15_MINUTES_MS | HOLD_60_MINUTES_MS)
                && started_at.checked_add(duration) == Some(until)
        }
        _ => false,
    }
}

fn status_from_loaded(document: Option<LoadedDocument>, now: i64) -> UserHoldStatus {
    match document {
        Some(LoadedDocument::Valid(document)) => status_from_document(&document, now),
        Some(LoadedDocument::Invalid) => UserHoldStatus {
            revision: 0,
            started_at: None,
            until: None,
            requested_duration: None,
            state: UserHoldState::Uncertain,
        },
        None => UserHoldStatus {
            revision: 0,
            started_at: None,
            until: None,
            requested_duration: None,
            state: UserHoldState::Inactive,
        },
    }
}

fn status_from_document(document: &HoldDocument, now: i64) -> UserHoldStatus {
    let state = match (document.started_at, document.until) {
        (Some(started_at), Some(_)) if now < started_at => UserHoldState::Uncertain,
        (Some(_), Some(until)) if now < until => UserHoldState::Active,
        _ => UserHoldState::Inactive,
    };
    UserHoldStatus {
        revision: document.revision,
        started_at: document.started_at,
        until: document.until,
        requested_duration: document.requested_duration,
        state,
    }
}

fn next_revision(revision: u64) -> Result<u64, AppError> {
    revision
        .checked_add(1)
        .filter(|value| *value <= MAX_SAFE_INTEGER)
        .ok_or_else(|| AppError::new("HOLD_REVISION_EXHAUSTED", "通知暂缓版本已达到上限"))
}

fn store_tx(tx: &Transaction<'_>, document: &HoldDocument) -> Result<(), AppError> {
    let encoded = serde_json::to_string(document)
        .map_err(|_| AppError::new("HOLD_DOCUMENT_INVALID", "通知暂缓状态无效"))?;
    tx.execute(
        "INSERT INTO app_metadata(key, value) VALUES(?, ?)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![HOLD_METADATA_KEY, encoded],
    )
    .map_err(AppError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::{Context, Waker};

    fn controller() -> HoldController {
        let db = Arc::new(Db::open_in_memory().unwrap());
        db.with_connection(|conn| {
            conn.execute("CREATE TABLE IF NOT EXISTS app_metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL)", [])?;
            Ok(())
        }).unwrap();
        HoldController::new(db)
    }

    #[test]
    fn hold_is_cas_bound_and_expires_across_restart_time() {
        let controller = controller();
        let receipt = controller.hold(0, HoldDuration::Minutes15, 1_000).unwrap();
        assert_eq!(receipt.status, "applied");
        assert_eq!(receipt.state.state, UserHoldState::Active);
        assert_eq!(
            controller
                .status_at(1_000 + HOLD_15_MINUTES_MS - 1)
                .unwrap()
                .state,
            UserHoldState::Active
        );
        assert_eq!(
            controller
                .status_at(1_000 + HOLD_15_MINUTES_MS)
                .unwrap()
                .state,
            UserHoldState::Inactive
        );
        assert_eq!(
            controller
                .hold(0, HoldDuration::Minutes60, 2_000)
                .unwrap()
                .status,
            "conflict"
        );
    }

    #[test]
    fn corrupt_and_clock_rollback_fail_closed_until_explicit_resume() {
        let controller = controller();
        let held = controller.hold(0, HoldDuration::Minutes15, 10_000).unwrap();
        assert_eq!(
            controller.status_at(9_999).unwrap().state,
            UserHoldState::Uncertain
        );
        assert_eq!(
            controller
                .resume(held.state.revision, 10_001)
                .unwrap()
                .status,
            "applied"
        );
        controller
            .db
            .with_connection(|conn| {
                conn.execute(
                    "UPDATE app_metadata SET value = '{bad' WHERE key = ?",
                    [HOLD_METADATA_KEY],
                )?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            controller.status_at(20_000).unwrap().state,
            UserHoldState::Uncertain
        );
        assert_eq!(
            controller.resume(0, 20_000).unwrap().state.state,
            UserHoldState::Inactive
        );
    }

    #[test]
    fn manual_preflight_and_permit_share_the_same_hold_state() {
        let controller = controller();
        assert!(controller.require_not_held(100).is_ok());
        assert!(controller.acquire_send_permit(100).unwrap().is_some());
        controller.hold(0, HoldDuration::Minutes15, 101).unwrap();
        assert_eq!(
            controller.require_not_held(102).unwrap_err().code,
            "NOTIFICATION_HOLD_ACTIVE"
        );
        assert!(controller.acquire_send_permit(102).unwrap().is_none());
    }

    #[test]
    fn committed_hold_before_first_http_poll_rejects_without_polling_request() {
        let controller = controller();
        let permit = controller.acquire_send_permit(100).unwrap().unwrap();
        // A temporary OutboxRepository/RelaySender controller for the same Db
        // must share the gate, rather than seeing a stale private snapshot.
        let other_controller = HoldController::new(Arc::clone(&controller.db));
        other_controller
            .hold(0, HoldDuration::Minutes15, 101)
            .unwrap();
        let polls = AtomicUsize::new(0);
        let mut future = std::pin::pin!(permit.start_http(
            std::future::poll_fn(|_| {
                polls.fetch_add(1, Ordering::SeqCst);
                Poll::<()>::Ready(())
            }),
            || false,
        ));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(
            future.as_mut().poll(&mut context),
            Poll::Ready(Err(HttpStartError::Held))
        ));
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn first_http_poll_before_hold_remains_an_inflight_request() {
        let controller = controller();
        let permit = controller.acquire_send_permit(100).unwrap().unwrap();
        let ready = AtomicBool::new(false);
        let polls = AtomicUsize::new(0);
        let mut future = std::pin::pin!(permit.start_http(
            std::future::poll_fn(|_| {
                polls.fetch_add(1, Ordering::SeqCst);
                if ready.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }),
            || false,
        ));
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        assert!(matches!(future.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
        controller.hold(0, HoldDuration::Minutes15, 101).unwrap();
        ready.store(true, Ordering::SeqCst);
        assert!(matches!(
            future.as_mut().poll(&mut context),
            Poll::Ready(Ok(()))
        ));
    }
}

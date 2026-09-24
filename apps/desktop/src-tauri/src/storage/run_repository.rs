use rusqlite::{params, OptionalExtension, Row, Transaction};

use crate::error::AppError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentRunRecord {
    pub run_key: String,
    pub agent_kind: String,
    pub instance_id: String,
    pub conversation_key: Option<String>,
    pub parent_run_key: Option<String>,
    pub agent_id: Option<String>,
    pub agent_role: Option<String>,
    pub agent_label: String,
    pub status: String,
    pub outcome: String,
    pub completion_confidence: String,
    pub started_at: Option<i64>,
    pub settling_at: Option<i64>,
    pub settle_not_before: Option<i64>,
    pub settle_generation: i64,
    pub completed_at: Option<i64>,
    pub failed_at: Option<i64>,
    pub interrupted_at: Option<i64>,
    pub cancelled_at: Option<i64>,
    pub last_event_at: i64,
}

pub(crate) struct RunRepository;

impl RunRepository {
    pub(crate) fn find(
        tx: &Transaction<'_>,
        run_key: &str,
    ) -> Result<Option<AgentRunRecord>, AppError> {
        tx.query_row(
            "SELECT run_key, agent_kind, instance_id, conversation_key, parent_run_key,
                    agent_id, agent_role,
                    agent_label, status, outcome,
                    completion_confidence, started_at, settling_at, settle_not_before,
                    settle_generation,
                    completed_at, failed_at, interrupted_at, cancelled_at, last_event_at
             FROM agent_runs WHERE run_key = ?",
            [run_key],
            read_run,
        )
        .optional()
        .map_err(AppError::from)
    }

    pub(crate) fn find_due_settling(
        tx: &Transaction<'_>,
        now: i64,
        limit: u16,
    ) -> Result<Vec<AgentRunRecord>, AppError> {
        let mut statement = tx.prepare(
            "SELECT run_key, agent_kind, instance_id, conversation_key, parent_run_key,
                    agent_id, agent_role,
                    agent_label, status, outcome,
                    completion_confidence, started_at, settling_at, settle_not_before,
                    settle_generation, completed_at, failed_at, interrupted_at, cancelled_at,
                    last_event_at
             FROM agent_runs
             WHERE status = 'settling' AND settle_not_before <= ?
             ORDER BY settle_not_before ASC, run_key ASC
             LIMIT ?",
        )?;
        let runs = statement
            .query_map(params![now, i64::from(limit)], read_run)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(AppError::from)?;
        Ok(runs)
    }

    pub(crate) fn next_settle_deadline(tx: &Transaction<'_>) -> Result<Option<i64>, AppError> {
        tx.query_row(
            "SELECT MIN(settle_not_before) FROM agent_runs WHERE status = 'settling'",
            [],
            |row| row.get(0),
        )
        .map_err(AppError::from)
    }

    /// Persists an already-reduced run projection without applying lifecycle policy.
    /// Existing rows can only be updated by the same immutable agent/instance identity.
    pub(crate) fn upsert(tx: &Transaction<'_>, run: &AgentRunRecord) -> Result<(), AppError> {
        let changed = tx.execute(
            "INSERT INTO agent_runs (
                run_key, agent_kind, instance_id, conversation_key, parent_run_key,
                agent_id, agent_role,
                agent_label, status, outcome,
                completion_confidence, started_at, settling_at, settle_not_before,
                settle_generation,
                completed_at, failed_at, interrupted_at, cancelled_at, last_event_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(run_key) DO UPDATE SET
                conversation_key = excluded.conversation_key,
                parent_run_key = excluded.parent_run_key,
                agent_id = excluded.agent_id,
                agent_role = excluded.agent_role,
                agent_label = excluded.agent_label,
                status = excluded.status,
                outcome = excluded.outcome,
                completion_confidence = excluded.completion_confidence,
                started_at = excluded.started_at,
                settling_at = excluded.settling_at,
                settle_not_before = excluded.settle_not_before,
                settle_generation = excluded.settle_generation,
                completed_at = excluded.completed_at,
                failed_at = excluded.failed_at,
                interrupted_at = excluded.interrupted_at,
                cancelled_at = excluded.cancelled_at,
                last_event_at = excluded.last_event_at
             WHERE agent_runs.agent_kind = excluded.agent_kind
               AND agent_runs.instance_id = excluded.instance_id",
            params![
                run.run_key,
                run.agent_kind,
                run.instance_id,
                run.conversation_key,
                run.parent_run_key,
                run.agent_id,
                run.agent_role,
                run.agent_label,
                run.status,
                run.outcome,
                run.completion_confidence,
                run.started_at,
                run.settling_at,
                run.settle_not_before,
                run.settle_generation,
                run.completed_at,
                run.failed_at,
                run.interrupted_at,
                run.cancelled_at,
                run.last_event_at,
            ],
        )?;
        if changed != 1 {
            return Err(AppError::new(
                "AGENT_RUN_IDENTITY_CONFLICT",
                "Run 标识已属于另一个 Agent 实例",
            ));
        }
        Ok(())
    }
}

pub(super) fn read_run(row: &Row<'_>) -> rusqlite::Result<AgentRunRecord> {
    Ok(AgentRunRecord {
        run_key: row.get(0)?,
        agent_kind: row.get(1)?,
        instance_id: row.get(2)?,
        conversation_key: row.get(3)?,
        parent_run_key: row.get(4)?,
        agent_id: row.get(5)?,
        agent_role: row.get(6)?,
        agent_label: row.get(7)?,
        status: row.get(8)?,
        outcome: row.get(9)?,
        completion_confidence: row.get(10)?,
        started_at: row.get(11)?,
        settling_at: row.get(12)?,
        settle_not_before: row.get(13)?,
        settle_generation: row.get(14)?,
        completed_at: row.get(15)?,
        failed_at: row.get(16)?,
        interrupted_at: row.get(17)?,
        cancelled_at: row.get(18)?,
        last_event_at: row.get(19)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn run(agent_kind: &str) -> AgentRunRecord {
        AgentRunRecord {
            run_key: "run-1".into(),
            agent_kind: agent_kind.into(),
            instance_id: "default".into(),
            conversation_key: Some("conversation-1".into()),
            parent_run_key: None,
            agent_id: Some("agent-private-1".into()),
            agent_role: Some("reviewer".into()),
            agent_label: "Fixture Agent".into(),
            status: "running".into(),
            outcome: "unknown".into(),
            completion_confidence: "provisional".into(),
            started_at: Some(10),
            settling_at: None,
            settle_not_before: None,
            settle_generation: 0,
            completed_at: None,
            failed_at: None,
            interrupted_at: None,
            cancelled_at: None,
            last_event_at: 10,
        }
    }

    #[test]
    fn upsert_round_trips_the_complete_projection() {
        let db = Db::open_in_memory().unwrap();
        let mut expected = run("fixture-agent");
        db.with_transaction(|tx| RunRepository::upsert(tx, &expected))
            .unwrap();
        expected.status = "settling".into();
        expected.settling_at = Some(20);
        expected.settle_not_before = Some(1_020);
        expected.settle_generation = 1;
        expected.last_event_at = 20;
        db.with_transaction(|tx| RunRepository::upsert(tx, &expected))
            .unwrap();
        let actual = db
            .with_transaction(|tx| RunRepository::find(tx, "run-1"))
            .unwrap()
            .unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn immutable_run_identity_cannot_be_rebound() {
        let db = Db::open_in_memory().unwrap();
        db.with_transaction(|tx| RunRepository::upsert(tx, &run("fixture-agent")))
            .unwrap();
        let error = db
            .with_transaction(|tx| RunRepository::upsert(tx, &run("other-agent")))
            .unwrap_err();
        assert_eq!(error.code, "AGENT_RUN_IDENTITY_CONFLICT");
    }
}

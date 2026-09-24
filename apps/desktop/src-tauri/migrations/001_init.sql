CREATE TABLE source_streams (
    source_key TEXT PRIMARY KEY CHECK (length(trim(source_key)) > 0),
    agent_kind TEXT NOT NULL CHECK (length(trim(agent_kind)) > 0),
    source_id TEXT NOT NULL CHECK (length(trim(source_id)) > 0),
    source_kind TEXT NOT NULL CHECK (length(trim(source_kind)) > 0),
    instance_id TEXT NOT NULL CHECK (length(trim(instance_id)) > 0),
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    UNIQUE (agent_kind, source_id, source_kind, instance_id)
);
CREATE TABLE source_checkpoints (
    source_key TEXT PRIMARY KEY REFERENCES source_streams(source_key) ON DELETE CASCADE,
    cursor_json TEXT NOT NULL CHECK (json_valid(cursor_json)), source_revision TEXT,
    status TEXT NOT NULL CHECK (status IN ('configured', 'active', 'degraded', 'unsupported')),
    last_error_code TEXT, updated_at INTEGER NOT NULL
);
CREATE TABLE agent_events (
    id TEXT PRIMARY KEY, source_key TEXT NOT NULL REFERENCES source_streams(source_key),
    event_id TEXT NOT NULL CHECK (length(trim(event_id)) > 0),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('prompt_submitted', 'run_started', 'run_settling', 'run_completed', 'run_failed', 'run_interrupted', 'run_cancelled', 'output_produced', 'attention_required')),
    conversation_key TEXT, run_key TEXT, parent_run_key TEXT, agent_id TEXT, agent_role TEXT,
    outcome TEXT CHECK (outcome IS NULL OR outcome IN ('success', 'failure', 'interrupted', 'cancelled', 'unknown')),
    completion_confidence TEXT CHECK (completion_confidence IS NULL OR completion_confidence IN ('authoritative', 'definitive_adapter', 'inferred', 'provisional')),
    occurred_at INTEGER NOT NULL, observed_at INTEGER NOT NULL, last_observed_at INTEGER NOT NULL DEFAULT 0,
    retention_class TEXT NOT NULL DEFAULT 'normal' CHECK (retention_class IN ('normal', 'unknown', 'verification')),
    content_scrubbed_at INTEGER,
    metadata_json TEXT NOT NULL, payload_hash TEXT NOT NULL CHECK (length(trim(payload_hash)) > 0),
    UNIQUE (source_key, event_id)
);
CREATE TABLE agent_runs (
    run_key TEXT PRIMARY KEY CHECK (length(trim(run_key)) > 0),
    agent_kind TEXT NOT NULL CHECK (length(trim(agent_kind)) > 0), instance_id TEXT NOT NULL CHECK (length(trim(instance_id)) > 0),
    conversation_key TEXT, parent_run_key TEXT, agent_id TEXT, agent_role TEXT,
    agent_label TEXT NOT NULL CHECK (length(trim(agent_label)) > 0),
    status TEXT NOT NULL CHECK (status IN ('running', 'settling', 'completed', 'failed', 'interrupted', 'cancelled', 'abandoned', 'unknown')),
    outcome TEXT NOT NULL DEFAULT 'unknown' CHECK (outcome IN ('success', 'failure', 'interrupted', 'cancelled', 'unknown')),
    completion_confidence TEXT NOT NULL DEFAULT 'provisional' CHECK (completion_confidence IN ('authoritative', 'definitive_adapter', 'inferred', 'provisional')),
    started_at INTEGER, settling_at INTEGER, settle_not_before INTEGER,
    settle_generation INTEGER NOT NULL DEFAULT 0 CHECK (settle_generation >= 0),
    completed_at INTEGER, failed_at INTEGER, interrupted_at INTEGER, cancelled_at INTEGER, last_event_at INTEGER NOT NULL,
    CHECK (completed_at IS NULL OR started_at IS NULL OR started_at <= completed_at)
);
CREATE TABLE agent_outputs (
    id TEXT PRIMARY KEY, run_key TEXT NOT NULL CHECK (length(trim(run_key)) > 0),
    source_key TEXT NOT NULL REFERENCES source_streams(source_key), event_id TEXT NOT NULL CHECK (length(trim(event_id)) > 0),
    output_kind TEXT NOT NULL CHECK (output_kind IN ('assistant_final', 'assistant_candidate', 'error_summary', 'system_summary')),
    raw_text TEXT NOT NULL, content_hash TEXT NOT NULL CHECK (length(trim(content_hash)) > 0),
    content_mode TEXT NOT NULL CHECK (content_mode IN ('status_only', 'redacted_excerpt', 'full_final')),
    content_available INTEGER NOT NULL CHECK (content_available IN (0, 1)),
    content_bytes INTEGER NOT NULL CHECK (content_bytes >= 0 AND content_bytes <= 262144),
    result_revision INTEGER NOT NULL CHECK (result_revision > 0),
    is_final INTEGER NOT NULL DEFAULT 0 CHECK (is_final IN (0, 1)), occurred_at INTEGER NOT NULL, observed_at INTEGER NOT NULL,
    UNIQUE (source_key, event_id), UNIQUE (run_key, result_revision)
);
CREATE TABLE attention_events (
    id TEXT PRIMARY KEY REFERENCES agent_events(id) ON DELETE CASCADE, run_key TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('permission', 'user_input', 'confirmation')),
    safe_summary TEXT NOT NULL CHECK (length(trim(safe_summary)) BETWEEN 1 AND 512),
    occurred_at INTEGER NOT NULL CHECK (occurred_at >= 0), expires_at INTEGER CHECK (expires_at IS NULL OR expires_at > occurred_at),
    resolution TEXT CHECK (resolution IS NULL OR length(trim(resolution)) BETWEEN 1 AND 128)
);
CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY, agent_run_key TEXT REFERENCES agent_runs(run_key) ON DELETE CASCADE,
    event_kind TEXT NOT NULL CHECK (event_kind IN ('activation', 'test', 'run_started', 'run_completed', 'run_failed', 'run_interrupted', 'attention_required')),
    dedupe_key TEXT NOT NULL UNIQUE, client_id TEXT NOT NULL UNIQUE, payload_json TEXT NOT NULL, content_policy_hash TEXT,
    list_title TEXT NOT NULL DEFAULT '通知记录' CHECK (length(list_title) BETWEEN 1 AND 160),
    list_content_mode TEXT NOT NULL DEFAULT 'status_only' CHECK (list_content_mode IN ('status_only', 'redacted_excerpt', 'full_final')),
    list_content_bytes INTEGER NOT NULL DEFAULT 0 CHECK (list_content_bytes >= 0 AND list_content_bytes <= 262144),
    list_result_revision INTEGER CHECK(list_result_revision BETWEEN 1 AND 9007199254740991),
    list_body_available INTEGER NOT NULL DEFAULT 0 CHECK (list_body_available IN (0, 1)),
    content_scrubbed_at INTEGER,
    delivery_backend TEXT NOT NULL DEFAULT 'relay' CHECK (delivery_backend = 'relay'),
    priority INTEGER NOT NULL, status TEXT NOT NULL CHECK (status IN ('pending', 'sending', 'retry_wait', 'blocked_activation', 'blocked_reconnect', 'delivered', 'expired', 'cancelled', 'dead_letter')),
    not_before INTEGER NOT NULL, expires_at INTEGER NOT NULL, attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    claimed_at INTEGER, next_attempt_at INTEGER, last_error_code TEXT, last_error_message TEXT, provider_message_id TEXT,
    acceptance_stage TEXT CHECK (acceptance_stage IS NULL OR acceptance_stage IN ('provider', 'relay')),
    relay_notification_id TEXT, relay_accepted_at INTEGER, remote_status TEXT, remote_updated_at INTEGER, remote_provider_message_id TEXT,
    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, delivered_at INTEGER,
    CHECK ((event_kind IN ('activation', 'test') AND agent_run_key IS NULL) OR (event_kind IN ('run_started', 'run_completed', 'run_failed', 'run_interrupted') AND agent_run_key IS NOT NULL) OR event_kind = 'attention_required')
);
CREATE TABLE app_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
INSERT INTO app_metadata (key, value) VALUES ('schema_identity', 'promptdock-desktop-v7');
INSERT INTO app_metadata (key, value) VALUES ('schema_revision', '1');
CREATE TABLE desktop_privacy (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  capture_mode TEXT NOT NULL DEFAULT 'status_only' CHECK (capture_mode IN ('status_only', 'redacted_excerpt', 'full_final')),
  content_epoch INTEGER NOT NULL DEFAULT 0 CHECK (content_epoch >= 0),
  policy_revision INTEGER NOT NULL DEFAULT 0 CHECK (policy_revision >= 0),
  task_input_enabled INTEGER NOT NULL DEFAULT 0 CHECK (task_input_enabled IN (0, 1)),
  excerpt_floor INTEGER NOT NULL CHECK (excerpt_floor >= 0)
);
INSERT INTO desktop_privacy(singleton, capture_mode, content_epoch, excerpt_floor)
VALUES (1, 'status_only', 0, 0);
CREATE INDEX idx_agent_runs_status_settle ON agent_runs (status, settle_not_before);
CREATE INDEX idx_agent_runs_recent ON agent_runs (last_event_at DESC);
CREATE INDEX idx_agent_runs_parent ON agent_runs(parent_run_key, last_event_at DESC, run_key DESC);
CREATE INDEX idx_agent_runs_agent_identity ON agent_runs(agent_kind, instance_id, agent_id, last_event_at DESC);
CREATE INDEX idx_agent_outputs_run ON agent_outputs (run_key, occurred_at DESC);
CREATE INDEX idx_agent_events_run ON agent_events (run_key, occurred_at ASC);
CREATE INDEX idx_agent_events_conversation ON agent_events (conversation_key, occurred_at ASC);
CREATE INDEX idx_attention_events_run ON attention_events (run_key, occurred_at DESC, id DESC);
CREATE INDEX idx_notification_outbox_ready ON notification_outbox (status, COALESCE(next_attempt_at, not_before), priority DESC, created_at ASC);
CREATE INDEX idx_notification_outbox_agent_run ON notification_outbox (agent_run_key, event_kind);
CREATE INDEX idx_notification_outbox_run_delivery ON notification_outbox (agent_run_key, created_at DESC, id DESC) WHERE agent_run_key IS NOT NULL;
CREATE INDEX idx_notification_outbox_provider_message ON notification_outbox(provider_message_id) WHERE provider_message_id IS NOT NULL;
CREATE INDEX idx_notification_outbox_retention_closed ON notification_outbox (status, updated_at ASC, id ASC) WHERE status IN ('cancelled', 'expired', 'dead_letter');
CREATE INDEX idx_notification_outbox_retention_delivered ON notification_outbox (delivered_at ASC, id ASC) WHERE status = 'delivered';

CREATE TABLE retention_counters (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    event_count INTEGER NOT NULL DEFAULT 0 CHECK (event_count >= 0),
    output_count INTEGER NOT NULL DEFAULT 0 CHECK (output_count >= 0),
    outbox_count INTEGER NOT NULL DEFAULT 0 CHECK (outbox_count >= 0),
    content_bytes INTEGER NOT NULL DEFAULT 0 CHECK (content_bytes >= 0)
);
INSERT INTO retention_counters(singleton) VALUES (1);
CREATE TRIGGER retention_events_insert AFTER INSERT ON agent_events BEGIN
    UPDATE retention_counters SET event_count = event_count + 1 WHERE singleton = 1;
END;
CREATE TRIGGER retention_events_delete AFTER DELETE ON agent_events BEGIN
    UPDATE retention_counters SET event_count = event_count - 1 WHERE singleton = 1;
END;
CREATE TRIGGER retention_outputs_insert AFTER INSERT ON agent_outputs BEGIN
    UPDATE retention_counters
    SET output_count = output_count + 1,
        content_bytes = content_bytes + NEW.content_bytes
    WHERE singleton = 1;
END;
CREATE TRIGGER retention_outputs_delete AFTER DELETE ON agent_outputs BEGIN
    UPDATE retention_counters
    SET output_count = output_count - 1,
        content_bytes = content_bytes - OLD.content_bytes
    WHERE singleton = 1;
END;
CREATE TRIGGER retention_outputs_scrub AFTER UPDATE OF content_bytes ON agent_outputs BEGIN
    UPDATE retention_counters
    SET content_bytes = content_bytes + NEW.content_bytes - OLD.content_bytes
    WHERE singleton = 1;
END;
CREATE TRIGGER retention_outbox_insert AFTER INSERT ON notification_outbox BEGIN
    UPDATE retention_counters
    SET outbox_count = outbox_count + 1,
        content_bytes = content_bytes + NEW.list_content_bytes
    WHERE singleton = 1;
END;
CREATE TRIGGER retention_outbox_delete AFTER DELETE ON notification_outbox BEGIN
    UPDATE retention_counters
    SET outbox_count = outbox_count - 1,
        content_bytes = content_bytes - CASE
            WHEN OLD.content_scrubbed_at IS NULL THEN OLD.list_content_bytes
            ELSE 0
        END
    WHERE singleton = 1;
END;
CREATE TRIGGER retention_outbox_scrub AFTER UPDATE OF content_scrubbed_at ON notification_outbox
WHEN OLD.content_scrubbed_at IS NULL AND NEW.content_scrubbed_at IS NOT NULL BEGIN
    UPDATE retention_counters
    SET content_bytes = content_bytes - NEW.list_content_bytes
    WHERE singleton = 1;
END;
CREATE INDEX idx_agent_outputs_retention ON agent_outputs (run_key, observed_at ASC, id ASC);
CREATE INDEX idx_agent_events_retention ON agent_events (observed_at ASC, id ASC);
CREATE INDEX idx_agent_runs_retention ON agent_runs (status, last_event_at ASC, run_key ASC);

CREATE TABLE relay_result_publications (
    outbox_id TEXT PRIMARY KEY REFERENCES notification_outbox(id) ON DELETE CASCADE,
    source_hash TEXT NOT NULL CHECK(length(source_hash) = 64),
    result_revision INTEGER NOT NULL CHECK(result_revision > 0),
    destination_identity TEXT NOT NULL CHECK(length(destination_identity) BETWEEN 1 AND 128),
    request_digest TEXT NOT NULL CHECK(length(request_digest) = 64),
    accepted_at INTEGER,
    page_state TEXT CHECK(page_state IN ('available', 'revoked', 'expired', 'content_unavailable')),
    page_expires_at INTEGER,
    notification_id TEXT,
    notification_status TEXT,
    updated_at INTEGER,
    pending_resend_request_id TEXT,
    pending_resend_notification_id TEXT,
    pending_revoke_request_id TEXT,
    CHECK((accepted_at IS NULL AND page_state IS NULL AND page_expires_at IS NULL AND notification_id IS NULL AND notification_status IS NULL AND updated_at IS NULL) OR (accepted_at IS NOT NULL AND page_state IS NOT NULL AND page_expires_at IS NOT NULL AND notification_id IS NOT NULL AND notification_status IS NOT NULL AND updated_at IS NOT NULL))
);

-- Fresh v7 only. Local reading state is never an Agent outcome or approval.
CREATE TABLE run_presentation (
    run_key TEXT PRIMARY KEY REFERENCES agent_runs(run_key) ON DELETE CASCADE,
    workspace_label TEXT NOT NULL DEFAULT '' CHECK(length(workspace_label) <= 120),
    activity_revision INTEGER NOT NULL DEFAULT 0 CHECK(activity_revision BETWEEN 0 AND 9007199254740991),
    attention_revision INTEGER NOT NULL DEFAULT 0 CHECK(attention_revision BETWEEN 0 AND 9007199254740991),
    acknowledged_attention_revision INTEGER NOT NULL DEFAULT 0 CHECK(acknowledged_attention_revision BETWEEN 0 AND attention_revision),
    last_attention_event_id TEXT,
    last_attention_at INTEGER,
    last_notice_at INTEGER,
    seen_result_revision INTEGER NOT NULL DEFAULT 0 CHECK(seen_result_revision BETWEEN 0 AND 9007199254740991),
    updated_at INTEGER NOT NULL
);
CREATE INDEX idx_activity_recent ON agent_runs(last_event_at DESC, run_key DESC);
CREATE INDEX idx_presentation_unread ON run_presentation(last_attention_at DESC, run_key DESC)
    WHERE attention_revision > acknowledged_attention_revision;
CREATE INDEX idx_result_activity ON relay_result_publications(result_revision DESC, outbox_id DESC);

-- PromptDock Relay v4 is a clean-break schema. Legacy databases are rejected
-- before any writable SQLite connection is opened; operators must use the
-- explicit reset script instead of attempting an in-place migration.

CREATE TABLE devices (
    id TEXT PRIMARY KEY CHECK(
        length(id) = 36
        AND substr(id, 9, 1) = '-'
        AND substr(id, 14, 1) = '-'
        AND substr(id, 19, 1) = '-'
        AND substr(id, 24, 1) = '-'
        AND replace(id, '-', '') NOT GLOB '*[^0-9a-f]*'
    ),
    name TEXT NOT NULL CHECK(length(trim(name)) BETWEEN 1 AND 80),
    token_hash BLOB NOT NULL CHECK(length(token_hash) = 32),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
    credential_version INTEGER NOT NULL DEFAULT 2 CHECK(credential_version = 2),
    created_at INTEGER NOT NULL,
    last_seen_at INTEGER,
    revoked_at INTEGER,
    rotated_at INTEGER
);
CREATE TABLE device_scopes (
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    scope TEXT NOT NULL CHECK(scope IN (
        'notify:write',
        'notify:read_own',
        'channel:read',
        'channel:manage',
        'gateway:connect',
        'job:query',
        'job:control'
    )),
    PRIMARY KEY(device_id, scope)
);
CREATE INDEX idx_device_scopes_scope ON device_scopes(scope, device_id);
CREATE TABLE app_metadata (
    key TEXT PRIMARY KEY CHECK(key IN ('schema_identity', 'schema_revision')),
    value TEXT NOT NULL
);
CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY,
    origin_kind TEXT NOT NULL CHECK(origin_kind IN ('device', 'system', 'admin')),
    origin_key TEXT NOT NULL,
    origin_device_id TEXT REFERENCES devices(id),
    notification_id TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    payload_hash TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_account_fingerprint TEXT,
    title TEXT NOT NULL CHECK(length(title) <= 128),
    body TEXT NOT NULL CHECK(length(body) <= 6000),
    body_kind TEXT NOT NULL DEFAULT 'plain' CHECK(body_kind IN ('plain', 'protected_result_link')),
    protected_body_ciphertext BLOB,
    protected_body_nonce BLOB,
    result_row_id TEXT,
    correlation_key TEXT,
    priority INTEGER NOT NULL CHECK(priority BETWEEN 0 AND 255),
    status TEXT NOT NULL CHECK(status IN (
        'pending_channel',
        'sending_channel',
        'retry_wait',
        'blocked_activation',
        'blocked_reconnect',
        'provider_accepted',
        'expired',
        'cancelled',
        'dead_letter'
    )),
    not_before INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    claim_token TEXT,
    claimed_at INTEGER,
    next_attempt_at INTEGER,
    last_error_code TEXT,
    last_error_message TEXT,
    provider_message_id TEXT,
    provider_accepted_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    sensitive_body INTEGER NOT NULL DEFAULT 0 CHECK(sensitive_body IN (0, 1)),
    CHECK(expires_at > created_at),
    CHECK(updated_at >= created_at),
    CHECK(
        target_account_fingerprint IS NULL
        OR (
            length(target_account_fingerprint) = 67
            AND substr(target_account_fingerprint, 1, 3) = 'wx:'
            AND substr(target_account_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
        )
    ),
    CHECK(
        (body_kind = 'plain' AND protected_body_ciphertext IS NULL
            AND protected_body_nonce IS NULL AND result_row_id IS NULL)
        OR (body_kind = 'protected_result_link' AND body = '[PROTECTED_RESULT_LINK]'
            AND protected_body_ciphertext IS NOT NULL AND length(protected_body_nonce) = 24
            AND result_row_id IS NOT NULL)
    ),
    CHECK(
        (origin_kind = 'device'
            AND origin_device_id IS NOT NULL
            AND origin_key = 'device:' || origin_device_id)
        OR (origin_kind = 'system'
            AND origin_device_id IS NULL
            AND origin_key LIKE 'system:%')
        OR (origin_kind = 'admin'
            AND origin_device_id IS NULL
            AND origin_key = 'admin:singleton')
    ),
    CHECK(
        (origin_kind = 'system'
            AND kind = 'interactive_reply'
            AND target_account_fingerprint IS NOT NULL)
        OR (origin_kind = 'device'
            AND (
                (kind = 'result_link' AND target_account_fingerprint IS NOT NULL)
                OR (kind <> 'interactive_reply' AND kind <> 'result_link'
                    AND target_account_fingerprint IS NULL)
            ))
        OR (origin_kind = 'admin'
            AND kind = 'test'
            AND target_account_fingerprint IS NULL)
    ),
    CHECK(
        (status = 'sending_channel' AND claim_token IS NOT NULL AND claimed_at IS NOT NULL)
        OR (status <> 'sending_channel' AND claim_token IS NULL AND claimed_at IS NULL)
    )
);
CREATE UNIQUE INDEX ux_outbox_origin_notification
ON notification_outbox(origin_key, notification_id);
CREATE UNIQUE INDEX ux_outbox_origin_dedupe
ON notification_outbox(origin_key, dedupe_key);
CREATE INDEX idx_outbox_ready
ON notification_outbox(
    status,
    COALESCE(next_attempt_at, not_before),
    priority DESC,
    created_at ASC
);
CREATE INDEX idx_outbox_retention
ON notification_outbox(status, updated_at, id);
CREATE INDEX idx_outbox_provider
ON notification_outbox(provider_message_id)
WHERE provider_message_id IS NOT NULL;
CREATE INDEX idx_admin_outbox_created
ON notification_outbox(created_at DESC, id DESC);
CREATE INDEX idx_admin_outbox_interactive_updated
ON notification_outbox(updated_at DESC, id DESC)
WHERE origin_kind = 'system' AND kind = 'interactive_reply';
CREATE INDEX idx_outbox_result_link ON notification_outbox(result_row_id)
WHERE result_row_id IS NOT NULL;
CREATE TABLE notification_bundles (
    id TEXT PRIMARY KEY,
    origin_key TEXT NOT NULL,
    origin_device_id TEXT NOT NULL REFERENCES devices(id),
    bundle_id TEXT NOT NULL CHECK(length(bundle_id) BETWEEN 1 AND 128),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 256),
    request_digest BLOB NOT NULL CHECK(length(request_digest) = 32),
    kind TEXT NOT NULL CHECK(kind = 'run_completed'),
    content_mode TEXT NOT NULL CHECK(content_mode = 'full_final'),
    source TEXT NOT NULL CHECK(source = 'codex_stop'),
    correlation_key TEXT NOT NULL CHECK(length(correlation_key) BETWEEN 1 AND 128),
    result_revision INTEGER NOT NULL CHECK(result_revision > 0),
    title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 128),
    body_ciphertext BLOB,
    body_nonce BLOB,
    body_bytes INTEGER NOT NULL CHECK(body_bytes BETWEEN 1 AND 262144),
    source_hash TEXT NOT NULL CHECK(
        length(source_hash) = 64 AND source_hash NOT GLOB '*[^0-9a-f]*'
    ),
    target_account_fingerprint TEXT NOT NULL CHECK(
        length(target_account_fingerprint) = 67
        AND substr(target_account_fingerprint, 1, 3) = 'wx:'
        AND substr(target_account_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    status TEXT NOT NULL CHECK(status IN (
        'queued', 'delivering', 'partial_failed', 'provider_accepted',
        'expired', 'blocked_target_changed', 'delivery_unknown'
    )),
    segment_count INTEGER NOT NULL CHECK(segment_count BETWEEN 1 AND 128),
    accepted_segments INTEGER NOT NULL DEFAULT 0 CHECK(
        accepted_segments >= 0 AND accepted_segments <= segment_count
    ),
    accepted_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    body_retain_until INTEGER NOT NULL,
    body_purged_at INTEGER,
    CHECK(origin_key = 'device:' || origin_device_id),
    CHECK(expires_at > created_at),
    CHECK(updated_at >= accepted_at),
    CHECK(body_retain_until >= accepted_at),
    CHECK(
        (body_purged_at IS NULL AND body_ciphertext IS NOT NULL AND length(body_nonce) = 24)
        OR (body_purged_at IS NOT NULL AND body_ciphertext IS NULL AND body_nonce IS NULL)
    )
);
CREATE UNIQUE INDEX ux_bundle_origin_id
ON notification_bundles(origin_key, bundle_id);
CREATE UNIQUE INDEX ux_bundle_origin_dedupe
ON notification_bundles(origin_key, dedupe_key);
CREATE INDEX idx_bundle_retention
ON notification_bundles(status, body_retain_until, updated_at, id);
CREATE TABLE notification_bundle_segments (
    id TEXT PRIMARY KEY,
    bundle_row_id TEXT NOT NULL REFERENCES notification_bundles(id) ON DELETE CASCADE,
    segment_index INTEGER NOT NULL CHECK(segment_index BETWEEN 0 AND 127),
    byte_start INTEGER NOT NULL CHECK(byte_start >= 0),
    byte_end INTEGER NOT NULL CHECK(byte_end > byte_start),
    content_hash TEXT NOT NULL CHECK(
        length(content_hash) = 64 AND content_hash NOT GLOB '*[^0-9a-f]*'
    ),
    rendered_header TEXT NOT NULL,
    client_id TEXT NOT NULL CHECK(length(client_id) BETWEEN 1 AND 128),
    status TEXT NOT NULL CHECK(status IN (
        'pending_channel', 'sending_channel', 'retry_wait',
        'blocked_activation', 'blocked_reconnect', 'blocked_target_changed',
        'provider_accepted', 'delivery_unknown', 'expired', 'dead_letter'
    )),
    not_before INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    claim_token TEXT,
    claimed_at INTEGER,
    next_attempt_at INTEGER,
    last_error_code TEXT,
    provider_message_id TEXT,
    provider_accepted_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK(expires_at > created_at),
    CHECK(updated_at >= created_at),
    CHECK(
        (status = 'sending_channel' AND claim_token IS NOT NULL AND claimed_at IS NOT NULL)
        OR (status <> 'sending_channel' AND claim_token IS NULL AND claimed_at IS NULL)
    ),
    UNIQUE(bundle_row_id, segment_index),
    UNIQUE(client_id)
);
CREATE INDEX idx_bundle_segment_ready
ON notification_bundle_segments(
    status, COALESCE(next_attempt_at, not_before), bundle_row_id, segment_index
);
CREATE INDEX idx_bundle_segment_parent_status
ON notification_bundle_segments(bundle_row_id, status, segment_index);
CREATE TABLE results (
    id TEXT PRIMARY KEY,
    origin_device_id TEXT NOT NULL REFERENCES devices(id),
    origin_key TEXT NOT NULL,
    result_id TEXT NOT NULL CHECK(length(result_id) BETWEEN 1 AND 128),
    dedupe_key TEXT NOT NULL CHECK(length(dedupe_key) BETWEEN 1 AND 256),
    request_digest BLOB NOT NULL CHECK(length(request_digest) = 32),
    kind TEXT NOT NULL CHECK(kind = 'run_completed'),
    content_mode TEXT NOT NULL CHECK(content_mode = 'full_final'),
    source TEXT NOT NULL CHECK(source = 'codex_stop'),
    correlation_key TEXT NOT NULL CHECK(length(correlation_key) BETWEEN 1 AND 128),
    result_revision TEXT NOT NULL CHECK(length(result_revision) BETWEEN 1 AND 20 AND result_revision NOT GLOB '*[^0-9]*'),
    safe_title TEXT NOT NULL CHECK(length(safe_title) BETWEEN 1 AND 128),
    body_ciphertext BLOB,
    body_nonce BLOB,
    body_bytes INTEGER NOT NULL CHECK(body_bytes BETWEEN 0 AND 262144),
    source_hash TEXT NOT NULL CHECK(length(source_hash) = 64 AND source_hash NOT GLOB '*[^0-9a-f]*'),
    started_at INTEGER,
    completed_at INTEGER,
    duration_ms INTEGER,
    accepted_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    page_expires_at INTEGER NOT NULL,
    body_retain_until INTEGER NOT NULL,
    body_purged_at INTEGER,
    revoked_at INTEGER,
    initial_notification_id TEXT NOT NULL,
    current_notification_id TEXT NOT NULL,
    notification_status TEXT NOT NULL DEFAULT 'pending_channel',
    updated_at INTEGER NOT NULL,
    CHECK(origin_key = 'device:' || origin_device_id),
    CHECK(page_expires_at > accepted_at),
    CHECK(body_retain_until >= page_expires_at),
    CHECK((body_purged_at IS NULL AND body_ciphertext IS NOT NULL AND length(body_nonce) = 24) OR (body_purged_at IS NOT NULL AND body_ciphertext IS NULL AND body_nonce IS NULL))
);
CREATE UNIQUE INDEX ux_results_origin_id ON results(origin_key, result_id);
CREATE UNIQUE INDEX ux_results_origin_dedupe ON results(origin_key, dedupe_key);
CREATE INDEX idx_results_retention ON results(page_expires_at, body_retain_until, updated_at);
CREATE TABLE result_shares (
    result_row_id TEXT PRIMARY KEY REFERENCES results(id) ON DELETE CASCADE,
    token_digest BLOB NOT NULL UNIQUE CHECK(length(token_digest) = 32),
    token_ciphertext BLOB NOT NULL,
    token_nonce BLOB NOT NULL CHECK(length(token_nonce) = 24),
    public_origin TEXT NOT NULL,
    share_revision INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
CREATE TABLE result_actions (
    result_row_id TEXT NOT NULL REFERENCES results(id) ON DELETE CASCADE,
    action_kind TEXT NOT NULL CHECK(action_kind IN ('revoke', 'resend')),
    request_id TEXT NOT NULL CHECK(length(request_id) BETWEEN 1 AND 128),
    notification_id TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(result_row_id, action_kind, request_id)
);
CREATE TABLE inbound_commands (
    message_key TEXT PRIMARY KEY CHECK(length(message_key) BETWEEN 1 AND 256),
    sender_fingerprint TEXT NOT NULL CHECK(
        length(sender_fingerprint) = 67
        AND substr(sender_fingerprint, 1, 3) = 'wx:'
        AND substr(sender_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    command_kind TEXT NOT NULL CHECK(command_kind IN (
        'help', 'list_devices', 'select_device', 'list_workspaces',
        'select_workspace', 'list_runtimes', 'select_runtime',
        'list_harness_profiles', 'select_harness_profile',
        'list_task_presets', 'start_run', 'confirm', 'confirmation_rejected',
        'cancel_confirmation', 'list_runs', 'next_page', 'get_run_status',
        'get_run_detail', 'get_run_tree', 'get_run_children', 'cancel_run',
        'unknown'
    )),
    command_json TEXT NOT NULL CHECK(
        json_valid(command_json)
        AND length(command_json) BETWEEN 1 AND 512
        AND json_type(command_json) = 'object'
        AND json_extract(command_json, '$.action') = command_kind
    ),
    payload_hash TEXT NOT NULL CHECK(
        length(payload_hash) = 64
        AND payload_hash NOT GLOB '*[^0-9a-f]*'
    ),
    status TEXT NOT NULL CHECK(status IN (
        'received', 'dispatching', 'waiting_gateway', 'reply_queued',
        'expired', 'dead_letter'
    )),
    claim_token TEXT,
    claimed_at INTEGER,
    reply_notification_id TEXT CHECK(
        reply_notification_id IS NULL OR length(reply_notification_id) BETWEEN 1 AND 128
    ),
    next_attempt_at INTEGER,
    expires_at INTEGER NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    last_error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK(next_attempt_at IS NULL OR next_attempt_at >= 0),
    CHECK(expires_at > created_at),
    CHECK(updated_at >= created_at),
    CHECK(
        (status IN ('dispatching', 'waiting_gateway')
            AND claim_token IS NOT NULL AND claimed_at IS NOT NULL)
        OR (status NOT IN ('dispatching', 'waiting_gateway')
            AND claim_token IS NULL AND claimed_at IS NULL)
    ),
    CHECK(
        (status = 'reply_queued' AND reply_notification_id IS NOT NULL)
        OR (status <> 'reply_queued' AND reply_notification_id IS NULL)
    )
);
CREATE INDEX idx_inbound_commands_ready
ON inbound_commands(status, expires_at, created_at, message_key);
CREATE INDEX idx_inbound_commands_claim
ON inbound_commands(status, claimed_at) WHERE claim_token IS NOT NULL;
CREATE INDEX idx_inbound_commands_retention
ON inbound_commands(status, updated_at, message_key);
CREATE INDEX idx_admin_inbound_created ON inbound_commands(created_at DESC);
CREATE INDEX idx_admin_inbound_reply_notification
ON inbound_commands(reply_notification_id) WHERE reply_notification_id IS NOT NULL;
CREATE TABLE selection_contexts (
    sender_fingerprint TEXT PRIMARY KEY CHECK(
        length(sender_fingerprint) = 67
        AND substr(sender_fingerprint, 1, 3) = 'wx:'
        AND substr(sender_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    selected_device TEXT REFERENCES devices(id),
    selected_runtime_handle TEXT CHECK(selected_runtime_handle IS NULL OR (
        length(selected_runtime_handle) BETWEEN 32 AND 128
        AND selected_runtime_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    selected_workspace_handle TEXT CHECK(selected_workspace_handle IS NULL OR (
        length(selected_workspace_handle) BETWEEN 32 AND 128
        AND selected_workspace_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    selected_harness_profile_handle TEXT CHECK(selected_harness_profile_handle IS NULL OR (
        length(selected_harness_profile_handle) BETWEEN 32 AND 128
        AND selected_harness_profile_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    query_kind TEXT CHECK(query_kind IS NULL OR query_kind IN (
        'devices', 'runtimes', 'workspaces', 'harness_profiles', 'task_presets',
        'runs_all', 'runs_recent', 'runs_failed'
    )),
    next_cursor TEXT CHECK(next_cursor IS NULL OR (
        length(next_cursor) BETWEEN 1 AND 512
        AND next_cursor NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    expires_at INTEGER NOT NULL
);
CREATE TABLE selection_entries (
    sender_fingerprint TEXT NOT NULL REFERENCES selection_contexts(sender_fingerprint)
        ON DELETE CASCADE,
    slot INTEGER NOT NULL CHECK(slot BETWEEN 1 AND 999),
    device_id TEXT NOT NULL REFERENCES devices(id),
    client_opaque_handle TEXT CHECK(client_opaque_handle IS NULL OR (
        length(client_opaque_handle) BETWEEN 32 AND 128
        AND client_opaque_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    item_kind TEXT NOT NULL CHECK(item_kind IN (
        'device', 'run', 'runtime', 'workspace', 'harness_profile', 'task_preset'
    )),
    PRIMARY KEY(sender_fingerprint, slot),
    CHECK(
        (item_kind = 'device' AND client_opaque_handle IS NULL)
        OR (item_kind <> 'device' AND client_opaque_handle IS NOT NULL)
    )
);
CREATE INDEX idx_selection_contexts_expiry
ON selection_contexts(expires_at, sender_fingerprint);
CREATE TABLE control_confirmations (
    confirmation_id TEXT PRIMARY KEY CHECK(
        length(confirmation_id) BETWEEN 1 AND 128
        AND confirmation_id NOT GLOB '*[^A-Za-z0-9_-]*'
    ),
    sender_fingerprint TEXT NOT NULL CHECK(
        length(sender_fingerprint) = 67
        AND substr(sender_fingerprint, 1, 3) = 'wx:'
        AND substr(sender_fingerprint, 4) NOT GLOB '*[^0-9a-f]*'
    ),
    confirmation_digest TEXT NOT NULL CHECK(
        length(confirmation_digest) = 64
        AND confirmation_digest NOT GLOB '*[^0-9a-f]*'
    ),
    confirmation_nonce TEXT NOT NULL CHECK(
        length(confirmation_nonce) = 32
        AND confirmation_nonce NOT GLOB '*[^0-9a-f]*'
    ),
    device_id TEXT NOT NULL REFERENCES devices(id),
    action_kind TEXT NOT NULL CHECK(action_kind IN ('start_run', 'cancel_run')),
    runtime_handle TEXT CHECK(runtime_handle IS NULL OR (
        length(runtime_handle) BETWEEN 32 AND 128
        AND runtime_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    workspace_handle TEXT CHECK(workspace_handle IS NULL OR (
        length(workspace_handle) BETWEEN 32 AND 128
        AND workspace_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    harness_profile_handle TEXT CHECK(harness_profile_handle IS NULL OR (
        length(harness_profile_handle) BETWEEN 32 AND 128
        AND harness_profile_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    task_preset_handle TEXT CHECK(task_preset_handle IS NULL OR (
        length(task_preset_handle) BETWEEN 32 AND 128
        AND task_preset_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    run_handle TEXT CHECK(run_handle IS NULL OR (
        length(run_handle) BETWEEN 32 AND 128
        AND run_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    intent_id TEXT NOT NULL CHECK(length(intent_id) BETWEEN 1 AND 128
        AND intent_id NOT GLOB '*[^A-Za-z0-9_-]*'),
    expires_at INTEGER NOT NULL CHECK(expires_at >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 5 CHECK(max_attempts BETWEEN 1 AND 10),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count BETWEEN 0 AND max_attempts),
    locked_at INTEGER CHECK(locked_at IS NULL OR locked_at >= 0),
    consumed_at INTEGER CHECK(consumed_at IS NULL OR (
        consumed_at >= 0 AND consumed_at < expires_at
    )),
    dispatch_state TEXT NOT NULL DEFAULT 'awaiting_confirmation' CHECK(
        dispatch_state IN ('awaiting_confirmation', 'locked', 'pending', 'outcome', 'failed', 'cancelled')
    ),
    dispatch_message_key TEXT UNIQUE REFERENCES inbound_commands(message_key) ON DELETE CASCADE,
    outcome_accepted INTEGER CHECK(outcome_accepted IS NULL OR outcome_accepted IN (0, 1)),
    outcome_run_handle TEXT CHECK(outcome_run_handle IS NULL OR (
        length(outcome_run_handle) BETWEEN 32 AND 128
        AND outcome_run_handle NOT GLOB '*[^A-Za-z0-9_-]*'
    )),
    outcome_status TEXT CHECK(outcome_status IS NULL OR (
        (action_kind = 'start_run' AND outcome_status IN (
            'queued', 'provisioning', 'starting', 'running', 'waiting_input', 'cancelling', 'terminal'
        ))
        OR (action_kind = 'cancel_run' AND outcome_status IN (
            'none', 'requested', 'cooperative_signal_sent', 'waiting_terminal',
            'closing_session', 'terminating_process_tree', 'terminal', 'failed'
        ))
    )),
    outcome_error_code TEXT CHECK(outcome_error_code IS NULL OR
        length(outcome_error_code) BETWEEN 1 AND 128),
    CHECK(
        (action_kind = 'start_run' AND runtime_handle IS NOT NULL
            AND workspace_handle IS NOT NULL AND harness_profile_handle IS NOT NULL
            AND task_preset_handle IS NOT NULL AND run_handle IS NULL)
        OR (action_kind = 'cancel_run' AND runtime_handle IS NULL
            AND workspace_handle IS NULL AND harness_profile_handle IS NULL
            AND task_preset_handle IS NULL AND run_handle IS NOT NULL)
    ),
    CHECK(
        (dispatch_state = 'awaiting_confirmation'
            AND attempt_count < max_attempts
            AND locked_at IS NULL
            AND consumed_at IS NULL
            AND dispatch_message_key IS NULL
            AND outcome_accepted IS NULL
            AND outcome_run_handle IS NULL
            AND outcome_status IS NULL
            AND outcome_error_code IS NULL)
        OR (dispatch_state = 'locked'
            AND attempt_count = max_attempts
            AND locked_at IS NOT NULL
            AND consumed_at IS NULL
            AND dispatch_message_key IS NULL
            AND outcome_accepted IS NULL
            AND outcome_run_handle IS NULL
            AND outcome_status IS NULL
            AND outcome_error_code IS NULL)
        OR (dispatch_state = 'pending'
            AND attempt_count < max_attempts
            AND locked_at IS NULL
            AND consumed_at IS NOT NULL
            AND dispatch_message_key IS NOT NULL
            AND outcome_accepted IS NULL
            AND outcome_run_handle IS NULL
            AND outcome_status IS NULL
            AND outcome_error_code IS NULL)
        OR (dispatch_state = 'outcome'
            AND attempt_count < max_attempts
            AND locked_at IS NULL
            AND consumed_at IS NOT NULL
            AND dispatch_message_key IS NOT NULL
            AND outcome_accepted IS NOT NULL
            AND outcome_run_handle IS NOT NULL
            AND outcome_status IS NOT NULL
            AND outcome_error_code IS NULL)
        OR (dispatch_state = 'failed'
            AND attempt_count < max_attempts
            AND locked_at IS NULL
            AND consumed_at IS NOT NULL
            AND dispatch_message_key IS NOT NULL
            AND outcome_accepted IS NULL
            AND outcome_run_handle IS NULL
            AND outcome_status IS NULL
            AND outcome_error_code IS NOT NULL)
        OR (dispatch_state = 'cancelled'
            AND attempt_count < max_attempts
            AND locked_at IS NULL
            AND consumed_at IS NULL
            AND dispatch_message_key IS NULL
            AND outcome_accepted IS NULL
            AND outcome_run_handle IS NULL
            AND outcome_status IS NULL
            AND outcome_error_code IS NULL)
    )
);
CREATE INDEX idx_control_confirmations_expiry
ON control_confirmations(expires_at, sender_fingerprint);
CREATE INDEX idx_control_confirmations_dispatch
ON control_confirmations(dispatch_state, dispatch_message_key);
CREATE UNIQUE INDEX idx_control_confirmations_one_pending_sender
ON control_confirmations(sender_fingerprint)
WHERE dispatch_state = 'awaiting_confirmation';

-- The owner receipt is a durable projection.  Keep its current-notification
-- state and revision monotonic when the channel worker advances that outbox
-- row, including two transitions within the same millisecond.
CREATE TRIGGER notification_outbox_current_result_status
AFTER UPDATE OF status, updated_at ON notification_outbox
WHEN NEW.result_row_id IS NOT NULL
 AND EXISTS (
     SELECT 1 FROM results r
     WHERE r.id = NEW.result_row_id
       AND r.current_notification_id = NEW.notification_id
 )
BEGIN
    UPDATE results
    SET notification_status = NEW.status,
        updated_at = CASE
            WHEN NEW.updated_at > updated_at THEN NEW.updated_at
            ELSE updated_at + 1
        END
    WHERE id = NEW.result_row_id
      AND current_notification_id = NEW.notification_id;
END;

INSERT INTO app_metadata(key, value) VALUES
    ('schema_identity', 'promptdock-relay-v4'),
    ('schema_revision', '3');

PRAGMA user_version = 3;

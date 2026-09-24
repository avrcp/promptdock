use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use relay_provider_wechat::{
    credentials::ConnectionBundle,
    http_client::WechatHttpError,
    protocol::{
        GetUpdatesResponse, MESSAGE_ITEM_TYPE_TEXT, MESSAGE_STATE_FINISH, MESSAGE_TYPE_USER,
        WeixinMessage,
    },
};
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::Zeroizing;

use crate::inbound::{EphemeralInboundText, InboundMessageError};
use crate::wechat::account_fingerprint;

use super::state::{
    ERROR_AUTHENTICATION_EXPIRED, ERROR_INVALID_ENDPOINT, ERROR_MONITOR_CAPTURE_FAILED,
    ERROR_MONITOR_NETWORK, ERROR_MONITOR_PERSIST_FAILED, ERROR_MONITOR_PROTOCOL,
    ERROR_MONITOR_REJECTED, MonitorState, Shared,
};

pub(super) const DEGRADED_AFTER_FAILURES: u32 = 3;
const MAX_TEXT_ITEMS: usize = 4;
const MAX_TEXT_CHARS: usize = 4_096;
const OVERSIZED_TEXT_CHARS: usize = MAX_TEXT_CHARS + 1;

pub(super) async fn run_monitor(
    shared: Arc<Shared>,
    mut bundle: ConnectionBundle,
    cancellation: CancellationToken,
) {
    let base_url = match Url::parse(&bundle.credentials.base_url) {
        Ok(url) => url,
        Err(_) => {
            set_terminal_failure(&shared, ERROR_INVALID_ENDPOINT);
            return;
        }
    };
    let mut failures = 0_u32;
    let mut retry_delay = shared.reconnect_delay;
    let mut reconnecting = false;
    let mut notification_active = false;

    'connect: loop {
        if cancellation.is_cancelled() {
            break;
        }
        match shared
            .transport
            .notify_start(&base_url, &bundle.credentials.bot_token, &cancellation)
            .await
        {
            Ok(()) => {
                notification_active = true;
                let Some(notify_activation) = set_active(&shared, &bundle) else {
                    break 'connect;
                };
                if notify_activation {
                    shared.hook.activated().await;
                }
                let recovered = std::mem::take(&mut reconnecting);
                if !is_monitor_active(&shared) {
                    break 'connect;
                }
                if recovered {
                    shared.hook.reconnected().await;
                }
                if !is_monitor_active(&shared) {
                    break 'connect;
                }
            }
            Err(WechatHttpError::Cancelled) if cancellation.is_cancelled() => break,
            Err(error) if is_terminal_error(&error) => {
                set_terminal_failure(&shared, error_code(&error));
                break;
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                reconnecting = true;
                set_retry_failure(&shared, failures, error_code(&error));
                if !wait_to_retry(&cancellation, retry_delay).await {
                    break;
                }
                retry_delay = next_delay(retry_delay, shared.max_reconnect_delay);
                continue;
            }
        }

        loop {
            let response = shared
                .transport
                .get_updates(
                    &base_url,
                    &bundle.credentials.bot_token,
                    &bundle.session.get_updates_buf,
                    &cancellation,
                )
                .await;
            match response {
                Ok(response) => {
                    set_last_poll(&shared, now_ms());
                    match apply_response(&shared, &bundle, response, &cancellation).await {
                        Ok(UpdateOutcome::Unchanged) => {
                            failures = 0;
                            retry_delay = shared.reconnect_delay;
                        }
                        Ok(UpdateOutcome::Saved {
                            next,
                            became_activated,
                        }) => {
                            failures = 0;
                            retry_delay = shared.reconnect_delay;
                            bundle = *next;
                            if became_activated {
                                shared.hook.activated().await;
                            }
                        }
                        Err(error) => {
                            failures = failures.saturating_add(1);
                            reconnecting = true;
                            set_retry_failure(&shared, failures, error.error_code());
                            if notification_active {
                                notify_stop_best_effort(&shared, &base_url, &bundle, &cancellation)
                                    .await;
                                notification_active = false;
                            }
                            if !wait_to_retry(&cancellation, retry_delay).await {
                                break 'connect;
                            }
                            retry_delay = next_delay(retry_delay, shared.max_reconnect_delay);
                            continue 'connect;
                        }
                    }
                }
                Err(WechatHttpError::Cancelled) if cancellation.is_cancelled() => break 'connect,
                Err(error) if is_terminal_error(&error) => {
                    set_terminal_failure(&shared, error_code(&error));
                    if notification_active {
                        notify_stop_best_effort(&shared, &base_url, &bundle, &cancellation).await;
                        notification_active = false;
                    }
                    break 'connect;
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    reconnecting = true;
                    set_retry_failure(&shared, failures, error_code(&error));
                    if notification_active {
                        notify_stop_best_effort(&shared, &base_url, &bundle, &cancellation).await;
                        notification_active = false;
                    }
                    if !wait_to_retry(&cancellation, retry_delay).await {
                        break 'connect;
                    }
                    retry_delay = next_delay(retry_delay, shared.max_reconnect_delay);
                    continue 'connect;
                }
            }
        }
    }

    if notification_active {
        notify_stop_best_effort(&shared, &base_url, &bundle, &CancellationToken::new()).await;
    }
    finish_monitor(&shared);
}

pub(super) fn finish_monitor(shared: &Shared) {
    let mut state = shared.lock_state();
    if !matches!(
        state.status.monitor,
        MonitorState::Degraded | MonitorState::ReconnectRequired
    ) {
        state.status.monitor = MonitorState::Stopped;
        state.status.error_code = None;
    }
    state.status.activated = false;
}

enum UpdateOutcome {
    Unchanged,
    Saved {
        next: Box<ConnectionBundle>,
        became_activated: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ApplyResponseError {
    Capture,
    Persist,
}

impl ApplyResponseError {
    const fn error_code(self) -> &'static str {
        match self {
            Self::Capture => ERROR_MONITOR_CAPTURE_FAILED,
            Self::Persist => ERROR_MONITOR_PERSIST_FAILED,
        }
    }
}

async fn apply_response(
    shared: &Shared,
    current: &ConnectionBundle,
    mut response: GetUpdatesResponse,
    cancellation: &CancellationToken,
) -> Result<UpdateOutcome, ApplyResponseError> {
    let now = now_ms();
    let mut next = current.clone();
    let mut changed = false;
    let mut inbound = false;
    let sender_fingerprint = account_fingerprint(&next.credentials.user_id);
    let next_cursor = response
        .get_updates_buf
        .take()
        .or_else(|| response.sync_buf.take());

    for mut message in response.msgs.take().unwrap_or_default() {
        if message.from_user_id.as_deref() != Some(next.credentials.user_id.as_str()) {
            continue;
        }
        let key = message_key(&message);
        let duplicate = key.as_ref().is_some_and(|key| {
            next.session
                .recent_message_keys
                .iter()
                .any(|existing| existing == key)
        });
        let is_finished_user = message.message_type == Some(MESSAGE_TYPE_USER)
            && message.message_state == Some(MESSAGE_STATE_FINISH);

        if is_finished_user
            && let Some(key) = key
            && let Some(text) = extract_text(&mut message)
        {
            let event = EphemeralInboundText {
                message_key: key.clone(),
                sender_fingerprint: sender_fingerprint.clone(),
                text,
                received_at: now,
            };
            shared
                .sink
                .accept(event, cancellation)
                .await
                .map_err(capture_error)?;
            inbound = true;
            if !duplicate {
                changed |= next.session.remember_message_key(key);
            }
        }

        if let Some(context_token) = message
            .context_token
            .take()
            .filter(|value| !value.is_empty())
        {
            next.session.context_token = Some(context_token);
            next.session.context_token_user_id = Some(next.credentials.user_id.clone());
            next.session.context_token_updated_at = Some(now);
            changed = true;
        }
    }

    // The provider cursor is deliberately applied only after every eligible
    // message has crossed the durable sink boundary. A sink failure therefore
    // leaves both the cursor and recent-message watermark replayable.
    if let Some(cursor) = next_cursor
        && next.session.get_updates_buf != cursor
    {
        next.session.get_updates_buf = cursor;
        changed = true;
    }

    if !changed {
        if inbound {
            set_last_inbound(shared, now);
        }
        return Ok(UpdateOutcome::Unchanged);
    }
    if next.validate().is_err() {
        return Err(ApplyResponseError::Persist);
    }
    let save_result = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(ApplyResponseError::Persist),
        result = shared.store.save(&next) => result,
    };
    if save_result.is_err() {
        return Err(ApplyResponseError::Persist);
    }

    let context_changed = next.session.context_token.as_deref()
        != current.session.context_token.as_deref()
        || next.session.context_token_user_id.as_deref()
            != current.session.context_token_user_id.as_deref();
    let mut state = shared.lock_state();
    let was_activated = state.status.activated;
    state.bundle = Some(next.clone());
    if context_changed {
        state.revision = state.revision.saturating_add(1);
        state.activation_required = false;
    }
    if inbound {
        state.status.last_inbound_at = Some(now);
    }
    let has_context = has_owned_context(&next);
    state.status.activated =
        state.status.monitor == MonitorState::Active && has_context && !state.activation_required;
    if state.status.activated {
        state.status.error_code = None;
    }
    let became_activated = !was_activated && state.status.activated;
    Ok(UpdateOutcome::Saved {
        next: Box::new(next),
        became_activated,
    })
}

fn extract_text(message: &mut WeixinMessage) -> Option<Zeroizing<String>> {
    let mut combined: Option<Zeroizing<String>> = None;
    let mut text_items = 0_usize;
    let mut chars = 0_usize;
    let mut oversized = false;
    let mut oversized_hasher =
        blake3::Hasher::new_derive_key("promptdock-relay/wechat-oversized-text/v1");

    for mut item in message.item_list.take().unwrap_or_default() {
        if item.item_type != Some(MESSAGE_ITEM_TYPE_TEXT) || item.is_completed == Some(false) {
            continue;
        }
        let Some(text) = item
            .text_item
            .as_mut()
            .and_then(|text_item| text_item.text.take())
            .map(Zeroizing::new)
            .filter(|text| !text.is_empty())
        else {
            continue;
        };

        text_items += 1;
        let separator_chars = usize::from(text_items > 1);
        chars = chars
            .saturating_add(separator_chars)
            .saturating_add(text.chars().count());
        oversized_hasher.update(&(text.len() as u64).to_be_bytes());
        oversized_hasher.update(text.as_bytes());

        if !oversized && (text_items > MAX_TEXT_ITEMS || chars > MAX_TEXT_CHARS) {
            oversized = true;
            combined = None;
        }
        if oversized {
            continue;
        }
        match &mut combined {
            Some(combined) => {
                combined.push('\n');
                combined.push_str(&text);
            }
            None => combined = Some(text),
        }
    }

    if oversized {
        let digest = oversized_hasher.finalize().to_hex();
        return Some(Zeroizing::new(format!(
            "{}:{digest}",
            "\u{fffd}".repeat(OVERSIZED_TEXT_CHARS)
        )));
    }
    combined
}

fn capture_error(_error: InboundMessageError) -> ApplyResponseError {
    ApplyResponseError::Capture
}

fn message_key(message: &WeixinMessage) -> Option<String> {
    if let Some(message_id) = message.message_id {
        return Some(format!("mid:{message_id}"));
    }
    if let Some(client_id) = message
        .client_id
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        let key = format!("cid:{client_id}");
        if key.len() <= 256 {
            return Some(key);
        }
    }
    message.seq.map(|seq| format!("seq:{seq}"))
}

/// Transitions a usable connection to Active. `None` means a concurrent
/// terminal transition already won and callers must not publish recovery hooks.
pub(super) fn set_active(shared: &Shared, bundle: &ConnectionBundle) -> Option<bool> {
    let mut state = shared.lock_state();
    let was_activated = state.status.activated;
    if state.status.monitor == MonitorState::ReconnectRequired {
        state.status.activated = false;
        return None;
    }
    state.status.monitor = MonitorState::Active;
    state.status.error_code = None;
    state.status.activated = has_owned_context(bundle) && !state.activation_required;
    Some(!was_activated && state.status.activated)
}

fn is_monitor_active(shared: &Shared) -> bool {
    shared.lock_state().status.monitor == MonitorState::Active
}

fn has_owned_context(bundle: &ConnectionBundle) -> bool {
    bundle.session.context_token.is_some()
        && bundle.session.context_token_user_id.as_deref()
            == Some(bundle.credentials.user_id.as_str())
}

pub(super) fn set_last_poll(shared: &Shared, at: i64) {
    let mut state = shared.lock_state();
    state.status.last_poll_at = Some(at);
    if state.status.monitor == MonitorState::ReconnectRequired {
        return;
    }
    state.status.monitor = MonitorState::Active;
    state.status.error_code = None;
}

fn set_last_inbound(shared: &Shared, at: i64) {
    shared.lock_state().status.last_inbound_at = Some(at);
}

pub(super) fn set_retry_failure(shared: &Shared, failures: u32, error_code: &'static str) {
    let mut state = shared.lock_state();
    if state.status.monitor == MonitorState::ReconnectRequired {
        return;
    }
    state.status.activated = false;
    state.status.monitor = if failures >= DEGRADED_AFTER_FAILURES {
        MonitorState::Degraded
    } else {
        MonitorState::Reconnecting
    };
    state.status.error_code = Some(error_code);
}

pub(super) fn set_terminal_failure(shared: &Shared, error_code: &'static str) {
    let mut state = shared.lock_state();
    state.status.activated = false;
    state.status.monitor = MonitorState::ReconnectRequired;
    state.status.error_code = Some(error_code);
}

pub(super) fn set_worker_failure(shared: &Shared, error_code: &'static str) {
    let mut state = shared.lock_state();
    if state.status.monitor == MonitorState::ReconnectRequired {
        return;
    }
    state.status.activated = false;
    state.status.monitor = MonitorState::Degraded;
    state.status.error_code = Some(error_code);
}

async fn notify_stop_best_effort(
    shared: &Shared,
    base_url: &Url,
    bundle: &ConnectionBundle,
    cancellation: &CancellationToken,
) {
    let _ = shared
        .transport
        .notify_stop(base_url, &bundle.credentials.bot_token, cancellation)
        .await;
}

async fn wait_to_retry(cancellation: &CancellationToken, delay: Duration) -> bool {
    tokio::select! {
        () = cancellation.cancelled() => false,
        () = tokio::time::sleep(delay) => true,
    }
}

fn next_delay(current: Duration, maximum: Duration) -> Duration {
    current.saturating_mul(2).min(maximum)
}

pub(super) fn is_terminal_error(error: &WechatHttpError) -> bool {
    error.is_stale_token()
        || matches!(
            error,
            WechatHttpError::InvalidEndpoint | WechatHttpError::HttpStatus(401 | 403)
        )
}

pub(super) fn error_code(error: &WechatHttpError) -> &'static str {
    match error {
        WechatHttpError::InvalidEndpoint => ERROR_INVALID_ENDPOINT,
        error if error.is_stale_token() => ERROR_AUTHENTICATION_EXPIRED,
        WechatHttpError::HttpStatus(401 | 403) => ERROR_AUTHENTICATION_EXPIRED,
        WechatHttpError::Transport(_) | WechatHttpError::Timeout => ERROR_MONITOR_NETWORK,
        WechatHttpError::ApiRejected { .. } | WechatHttpError::HttpStatus(_) => {
            ERROR_MONITOR_REJECTED
        }
        WechatHttpError::InvalidHeader
        | WechatHttpError::ClientConfiguration
        | WechatHttpError::ResponseTooLarge
        | WechatHttpError::InvalidJson
        | WechatHttpError::InvalidResponse
        | WechatHttpError::Cancelled => ERROR_MONITOR_PROTOCOL,
    }
}

fn now_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

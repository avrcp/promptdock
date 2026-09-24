use super::*;

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const UNSUPPORTED_POLL_INTERVAL: Duration = Duration::from_secs(5);
const HEALTH_REPORT_INTERVAL: Duration = Duration::from_secs(10);
// A 256 KiB result can expand under JSON escaping and base64-wrapped DPAPI.
// Keep the durable line bounded while allowing the 2 MiB Hook input budget.
pub(super) const MAX_INBOX_LINE_BYTES: usize = 3 * 1024 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LimitedLine {
    Complete(Vec<u8>),
    Oversized { consumed: u64 },
    Partial,
    Eof,
}

pub(super) enum InboxRead {
    Eof,
    Partial,
    Advance {
        record: Box<InboxRecord>,
        next_cursor: InboxCursor,
    },
    Unsupported {
        cursor: InboxCursor,
    },
}

#[derive(Debug, Deserialize)]
struct EnvelopeHeader {
    schema_version: u16,
}

#[derive(Default)]
pub(super) struct HealthThrottle {
    last_key: Option<(&'static str, &'static str)>,
    last_reported_at: Option<Instant>,
}

impl HealthThrottle {
    pub(super) fn should_report(&mut self, status: &'static str, code: &'static str) -> bool {
        let now = Instant::now();
        let due = self.last_key != Some((status, code))
            || self
                .last_reported_at
                .is_none_or(|last| now.duration_since(last) >= HEALTH_REPORT_INTERVAL);
        if due {
            self.last_key = Some((status, code));
            self.last_reported_at = Some(now);
        }
        due
    }

    pub(super) fn reset(&mut self) {
        self.last_key = None;
        self.last_reported_at = None;
    }
}

pub(super) struct InboxSourceContext {
    pub(super) db: Arc<Db>,
    pub(super) events: Arc<dyn RuntimeEventEmitter>,
    pub(super) paths: InboxPaths,
    pub(super) source: SourceDescriptor,
    pub(super) on_caught_up: Arc<dyn Fn() + Send + Sync>,
}

pub(super) fn run_inbox_source(
    context: InboxSourceContext,
    sender: mpsc::SyncSender<WorkItem>,
    control: mpsc::Receiver<RuntimeCommand>,
    cancellation: Arc<Cancellation>,
) {
    let InboxSourceContext {
        db,
        events,
        paths,
        source,
        on_caught_up,
    } = context;
    while let Err(error) = recover_maintenance_for_source(&db, &paths, &source) {
        tracing::error!(%error, "failed to recover inbox maintenance journal");
        if cancellation.is_cancelled() {
            return;
        }
        cancellation.wait(POLL_INTERVAL);
    }
    let mut cursor = loop {
        match load_cursor_for_source(&db, &source) {
            Ok(cursor) => break cursor,
            Err(error) => {
                tracing::error!(%error, "failed to load durable inbox cursor");
                if cancellation.is_cancelled() {
                    return;
                }
                cancellation.wait(HEALTH_REPORT_INTERVAL);
            }
        }
    };
    let mut health_throttle = HealthThrottle::default();
    while !cancellation.is_cancelled() {
        if paths.maintenance_journal().exists() {
            match recover_maintenance_for_source(&db, &paths, &source) {
                Ok(()) => match load_cursor_for_source(&db, &source) {
                    Ok(recovered_cursor) => cursor = recovered_cursor,
                    Err(error) => {
                        tracing::error!(%error, "failed to load recovered inbox cursor");
                        cancellation.wait(POLL_INTERVAL);
                        continue;
                    }
                },
                Err(error) => {
                    tracing::error!(%error, "pending inbox maintenance remains blocked");
                    cancellation.wait(POLL_INTERVAL);
                    continue;
                }
            }
        }
        match control.try_recv() {
            Ok(RuntimeCommand::Wake) => continue,
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        match read_next_record(paths.inbox(), &cursor) {
            Ok(InboxRead::Advance {
                record,
                next_cursor,
            }) => {
                health_throttle.reset();
                let record = enforce_source_identity(*record, &source);
                let checkpoint = match checkpoint_for_source(&next_cursor, &record, &source) {
                    Ok(checkpoint) => checkpoint,
                    Err(error) => {
                        tracing::error!(%error, "failed to prepare inbox checkpoint");
                        cancellation.wait(POLL_INTERVAL);
                        continue;
                    }
                };
                let (ack_sender, ack_receiver) = mpsc::sync_channel(0);
                if sender
                    .send(WorkItem {
                        record,
                        checkpoint,
                        ack: ack_sender,
                    })
                    .is_err()
                {
                    break;
                }
                match ack_receiver.recv() {
                    Ok(true) => cursor = next_cursor,
                    Ok(false) => {
                        cancellation.wait(POLL_INTERVAL);
                    }
                    Err(_) => break,
                }
            }
            Ok(InboxRead::Unsupported {
                cursor: halted_cursor,
            }) => {
                cursor = halted_cursor;
                if health_throttle.should_report("unsupported", "UNSUPPORTED_ENVELOPE_VERSION") {
                    let result = persist_source_health_for_source(
                        &db,
                        events.as_ref(),
                        &cursor,
                        "unsupported",
                        "UNSUPPORTED_ENVELOPE_VERSION",
                        &source,
                    );
                    if let Err(error) = result {
                        tracing::error!(%error, "failed to persist unsupported inbox health");
                    } else {
                        tracing::warn!(
                            code = "UNSUPPORTED_ENVELOPE_VERSION",
                            "inbox contains a newer durable envelope; consumption is paused"
                        );
                    }
                }
                drain_wake_commands(&control);
                cancellation.wait(UNSUPPORTED_POLL_INTERVAL);
            }
            Ok(InboxRead::Partial) => {
                drain_wake_commands(&control);
                cancellation.wait(POLL_INTERVAL);
            }
            Ok(InboxRead::Eof) => {
                if cursor.offset > 0 {
                    if let Err(error) = compact_at_eof_for_source(
                        &db,
                        &paths,
                        &mut cursor,
                        COMPACTION_THRESHOLD_BYTES,
                        &source,
                    ) {
                        tracing::warn!(%error, "automatic inbox compaction failed");
                    }
                }
                on_caught_up();
                match control.recv_timeout(POLL_INTERVAL) {
                    Ok(RuntimeCommand::Wake) => {}
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            Err(error) => {
                if health_throttle.should_report("degraded", error.code) {
                    if let Err(persist_error) = persist_source_health_for_source(
                        &db,
                        events.as_ref(),
                        &cursor,
                        "degraded",
                        error.code,
                        &source,
                    ) {
                        tracing::error!(%persist_error, "failed to persist inbox source error");
                    }
                    tracing::warn!(
                        code = error.code,
                        error = %error.source,
                        "inbox source is temporarily unavailable"
                    );
                }
                drain_wake_commands(&control);
                cancellation.wait(POLL_INTERVAL);
            }
        }
    }
}

fn drain_wake_commands(control: &mpsc::Receiver<RuntimeCommand>) {
    while matches!(control.try_recv(), Ok(RuntimeCommand::Wake)) {}
}

pub(super) fn read_next_record(
    path: &Path,
    cursor: &InboxCursor,
) -> Result<InboxRead, SourceReadError> {
    read_next_record_with_unprotect(path, cursor, |protected| {
        crate::content_crypto::unprotect_text(
            crate::content_crypto::ContentPurpose::InboxEvent,
            protected,
        )
    })
}

fn read_next_record_with_unprotect(
    path: &Path,
    cursor: &InboxCursor,
    unprotect: impl FnOnce(&str) -> Result<zeroize::Zeroizing<Vec<u8>>, AppError>,
) -> Result<InboxRead, SourceReadError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(InboxRead::Eof),
        Err(error) => return Err(source_error(error)),
    };
    let metadata = file.metadata().map_err(source_error)?;
    let identity = file_identity(&file, &metadata);
    let mut offset = cursor.offset;
    let identity_changed = matches!(
        (cursor.file_identity, identity),
        (Some(previous), Some(current)) if previous != current
    );
    let generation = if metadata.len() < offset || identity_changed {
        offset = 0;
        None
    } else {
        cursor.generation.clone()
    };
    file.seek(SeekFrom::Start(offset)).map_err(source_error)?;
    let mut reader = BufReader::new(file);
    let current_cursor = InboxCursor {
        generation,
        offset,
        file_identity: identity,
    };
    let (mut line, consumed) = match read_limited_line(&mut reader, MAX_INBOX_LINE_BYTES)
        .map_err(source_error)?
    {
        LimitedLine::Complete(line) => {
            let consumed = u64::try_from(line.len())
                .ok()
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| source_error(std::io::Error::other("inbox line length overflow")))?;
            (line, consumed)
        }
        LimitedLine::Oversized { consumed } => {
            let next_cursor = advanced_cursor(&current_cursor, consumed)?;
            return Ok(InboxRead::Advance {
                record: Box::new(InboxRecord::Rejected("OVERSIZED_ENVELOPE")),
                next_cursor,
            });
        }
        LimitedLine::Partial => return Ok(InboxRead::Partial),
        LimitedLine::Eof => return Ok(InboxRead::Eof),
    };

    if line.last() == Some(&b'\r') {
        line.pop();
    }
    let protected = match std::str::from_utf8(&line) {
        Ok(protected) => protected,
        Err(_) => {
            return Ok(InboxRead::Advance {
                record: Box::new(InboxRecord::Rejected("MALFORMED_ENCRYPTED_ENVELOPE")),
                next_cursor: advanced_cursor(&current_cursor, consumed)?,
            })
        }
    };
    let plaintext = match unprotect(protected) {
        Ok(plaintext) => plaintext,
        Err(error) if crate::content_crypto::is_protected_content_corrupt(&error) => {
            return Ok(InboxRead::Advance {
                record: Box::new(InboxRecord::Rejected("MALFORMED_ENCRYPTED_ENVELOPE")),
                next_cursor: advanced_cursor(&current_cursor, consumed)?,
            })
        }
        Err(error) => {
            return Err(SourceReadError {
                code: "PROTECTED_CONTENT_UNAVAILABLE",
                source: std::io::Error::other(error.message),
            })
        }
    };
    let header = match serde_json::from_slice::<EnvelopeHeader>(&plaintext) {
        Ok(header) => header,
        Err(_) => {
            return Ok(InboxRead::Advance {
                record: Box::new(InboxRecord::Rejected("MALFORMED_ENVELOPE")),
                next_cursor: advanced_cursor(&current_cursor, consumed)?,
            })
        }
    };
    if header.schema_version > AGENT_EVENT_SCHEMA_VERSION {
        return Ok(InboxRead::Unsupported {
            cursor: current_cursor,
        });
    }
    let record = match AgentEventEnvelopeV2::decode_json(&plaintext) {
        Ok(envelope) => InboxRecord::Envelope(Box::new(envelope)),
        Err(_) => InboxRecord::Rejected("MALFORMED_ENVELOPE"),
    };
    Ok(InboxRead::Advance {
        record: Box::new(record),
        next_cursor: advanced_cursor(&current_cursor, consumed)?,
    })
}

pub(super) fn read_limited_line(
    reader: &mut impl BufRead,
    max_line_bytes: usize,
) -> std::io::Result<LimitedLine> {
    let mut line = Vec::with_capacity(max_line_bytes.min(16 * 1024));
    let mut consumed = 0_u64;
    let mut oversized = false;

    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(if consumed == 0 {
                LimitedLine::Eof
            } else {
                LimitedLine::Partial
            });
        }

        if let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
            let bytes_before_newline = &buffer[..newline];
            if !oversized {
                let remaining = max_line_bytes.saturating_sub(line.len());
                if bytes_before_newline.len() <= remaining {
                    line.extend_from_slice(bytes_before_newline);
                } else {
                    oversized = true;
                }
            }
            let chunk_consumed = newline + 1;
            consumed = add_consumed(consumed, chunk_consumed)?;
            reader.consume(chunk_consumed);
            return Ok(if oversized {
                LimitedLine::Oversized { consumed }
            } else {
                LimitedLine::Complete(line)
            });
        }

        let chunk_len = buffer.len();
        if !oversized {
            let remaining = max_line_bytes.saturating_sub(line.len());
            if chunk_len <= remaining {
                line.extend_from_slice(buffer);
            } else {
                if remaining > 0 {
                    line.extend_from_slice(&buffer[..remaining]);
                }
                oversized = true;
            }
        }
        consumed = add_consumed(consumed, chunk_len)?;
        reader.consume(chunk_len);
    }
}

pub(super) fn add_consumed(current: u64, amount: usize) -> std::io::Result<u64> {
    let amount =
        u64::try_from(amount).map_err(|_| std::io::Error::other("inbox line length overflow"))?;
    current
        .checked_add(amount)
        .ok_or_else(|| std::io::Error::other("inbox line length overflow"))
}

pub(super) fn enforce_source_identity(
    record: InboxRecord,
    source: &SourceDescriptor,
) -> InboxRecord {
    match record {
        InboxRecord::Envelope(envelope) if envelope.source != *source => {
            InboxRecord::Rejected("SOURCE_IDENTITY_MISMATCH")
        }
        record => record,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::agent::{
        AgentCorrelationV2, AgentEventPayloadV2, RunSettlingV2, SettlingReason,
        AGENT_EVENT_SCHEMA_VERSION,
    };
    use crate::source::{AgentKind, SourceDescriptor};

    fn event() -> AgentEventEnvelopeV2 {
        AgentEventEnvelopeV2 {
            metadata: None,
            schema_version: AGENT_EVENT_SCHEMA_VERSION,
            source: SourceDescriptor::new(
                AgentKind::new("fixture").unwrap(),
                "events",
                "hook_inbox",
                "default",
            )
            .unwrap(),
            event_id: "event-1".into(),
            occurred_at: 1,
            observed_at: 1,
            correlation: AgentCorrelationV2 {
                run_key: Some("run-1".into()),
                ..AgentCorrelationV2::default()
            },
            payload: AgentEventPayloadV2::RunSettling(RunSettlingV2 {
                reason: SettlingReason::AgentStopHook,
                codex_usage: None,
            }),
        }
    }

    fn protected_event_line() -> String {
        crate::content_crypto::protect_json(
            crate::content_crypto::ContentPurpose::InboxEvent,
            &event(),
        )
        .unwrap()
    }

    #[test]
    fn temporary_unprotect_failure_retries_the_same_inbox_line() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("events.jsonl");
        std::fs::write(&inbox, format!("{}\n", protected_event_line())).unwrap();
        let cursor = InboxCursor::default();

        let error = read_next_record_with_unprotect(&inbox, &cursor, |_| {
            Err(AppError::new(
                "PROTECTED_CONTENT_UNAVAILABLE",
                "temporary fixture failure",
            ))
        })
        .err()
        .unwrap();
        assert_eq!(error.code, "PROTECTED_CONTENT_UNAVAILABLE");

        assert!(matches!(
            read_next_record(&inbox, &cursor).unwrap(),
            InboxRead::Advance { record, .. }
                if matches!(*record, InboxRecord::Envelope(_))
        ));
    }

    #[test]
    fn corrupt_ciphertext_advances_and_the_following_record_still_decodes() {
        let directory = tempfile::tempdir().unwrap();
        let inbox = directory.path().join("events.jsonl");
        let corrupt = crate::content_crypto::protect_text(
            crate::content_crypto::ContentPurpose::NotificationPayload,
            b"wrong purpose ciphertext",
        )
        .unwrap();
        std::fs::write(&inbox, format!("{corrupt}\n{}\n", protected_event_line())).unwrap();

        let InboxRead::Advance {
            record,
            next_cursor,
        } = read_next_record(&inbox, &InboxCursor::default()).unwrap()
        else {
            panic!("corrupt ciphertext must be rejected")
        };
        assert!(matches!(
            *record,
            InboxRecord::Rejected("MALFORMED_ENCRYPTED_ENVELOPE")
        ));
        assert!(matches!(
            read_next_record(&inbox, &next_cursor).unwrap(),
            InboxRead::Advance { record, .. }
                if matches!(*record, InboxRecord::Envelope(_))
        ));
    }
}

#![forbid(unsafe_code)]

//! Isolated protocol and transport boundary for the pinned WeChat iLink
//! compatibility reference.
//!
//! The crate deliberately owns no database, web server, UI, renderer, outbox,
//! login orchestration, or secret-store implementation.

pub mod credentials;
pub mod http_client;
pub mod protocol;
pub mod redaction;

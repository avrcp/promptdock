#![forbid(unsafe_code)]

//! Relay-owned provider boundary for the pinned WeChat iLink transport.
//!
//! Application and transport crates consume this facade instead of coupling
//! directly to the low-level protocol crate. This crate intentionally has no
//! SQLx, Axum, filesystem, or Relay application-state dependency.

pub use wechat_ilink::{credentials, http_client, protocol, redaction};

use http_client::{WechatHttpClient, WechatHttpError};

#[derive(Clone)]
pub struct ProductionWechatProvider {
    client: WechatHttpClient,
}

impl ProductionWechatProvider {
    pub fn new(bot_agent: impl Into<String>) -> Result<Self, WechatHttpError> {
        Ok(Self {
            client: WechatHttpClient::production(bot_agent)?,
        })
    }

    pub const fn client(&self) -> &WechatHttpClient {
        &self.client
    }

    pub fn into_client(self) -> WechatHttpClient {
        self.client
    }
}

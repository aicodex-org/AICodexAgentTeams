//! 官方本地 DevKit 的非官方 Rust 移植，独立于宿主与界面
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

#![doc = include_str!("../README.md")]

mod client;
mod errors;
pub mod hosted;
mod oauth;
mod responses;
mod storage;
pub mod transport;
mod types;

pub use client::{ChatGPT, Subscription};
pub use errors::ChatGPTError;
pub use tokio_util::sync::CancellationToken;
pub use types::*;

pub const CHATGPT_USAGE_URL: &str = "https://chatgpt.com/settings/usage";
pub(crate) const SHARING_SCOPE: &str = "chatgpt.tokens.use.direct";

#[cfg(test)]
#[path = "../tests/main.rs"]
mod tests;

//! 官方五组本地 SDK 合同与 Rust 生命周期回归测试入口
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

mod authorization;
mod errors;
mod hosted;
mod refresh;
mod responses;
mod restart;
mod storage;
mod support;
mod transport;
#[cfg(feature = "upstream-parity")]
mod upstream;

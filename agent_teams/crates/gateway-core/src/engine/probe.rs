//! 管理端账号连通性测试的真实执行链端口

use std::fmt;

use bytes::Bytes;
use futures::future::BoxFuture;

use crate::error::{ClientVisibleUpstreamResponse, GatewayError, GatewayErrorKind};
use crate::identity::ProviderKind;
use crate::routing::UpstreamModelId;
use crate::{
    account::ProviderAccountId,
    engine::EngineError,
    error::{ProviderError, ProviderErrorKind},
    event::{GatewayEvent, ProviderEvent},
    operation::{Operation, OperationKind},
    upstream::UpstreamSendState,
};

#[derive(Debug, Clone, PartialEq)]
pub struct AccountProbeRequest {
    pub account_id: ProviderAccountId,
    pub provider_kind: ProviderKind,
    pub upstream_model: UpstreamModelId,
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountProbeResult {
    pub text: Vec<String>,
}

impl AccountProbeResult {
    pub(crate) fn from_events(
        operation: OperationKind,
        protocol: &str,
        events: Vec<ProviderEvent>,
    ) -> Result<Self, EngineError> {
        if operation != OperationKind::StructuredInference {
            return Ok(Self {
                text: events
                    .into_iter()
                    .flat_map(|event| event.into_parts().0)
                    .filter_map(|fact| match fact {
                        GatewayEvent::TextDelta(delta) => Some(delta.text),
                        _ => None,
                    })
                    .collect(),
            });
        }
        // 结构化探测展示原生正文，不解析 Provider schema，也不合成聊天事实
        // 在下游提交前拒绝缺失、重复或无法安全展示的载荷
        const MAXIMUM_JSON_BYTES: usize = 4 * 1024 * 1024;
        let invalid = || {
            EngineError::Provider(
                ProviderError::new(ProviderErrorKind::Protocol, UpstreamSendState::Sent)
                    .with_retry_prohibited(),
            )
        };
        let mut text = None;
        for event in events {
            let (_, wire) = event.into_parts();
            let Some(wire) = wire else { continue };
            if text.is_some() || wire.protocol() != protocol {
                return Err(invalid());
            }
            let bytes = wire.into_raw_json_body().ok_or_else(invalid)?;
            if bytes.len() > MAXIMUM_JSON_BYTES {
                return Err(invalid());
            }
            let raw = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
            if raw.trim().is_empty() {
                return Err(invalid());
            }
            text = Some(raw.to_owned());
        }
        Ok(Self {
            text: vec![text.ok_or_else(invalid)?],
        })
    }
}

/// 仅供当前管理端连接测试展示的原始上游失败响应
///
/// 正文不进入 `Debug`、日志或持久化；Core 只在探测终态从原始 Provider 错误显式复制
#[derive(PartialEq, Eq)]
pub struct AccountProbeUpstreamResponse {
    status: u16,
    content_type: Option<Vec<u8>>,
    body: Bytes,
}

impl AccountProbeUpstreamResponse {
    pub(crate) fn from_client_response(response: &ClientVisibleUpstreamResponse) -> Self {
        Self {
            status: response.status(),
            content_type: response.content_type().map(<[u8]>::to_vec),
            body: response.body().clone(),
        }
    }

    #[must_use]
    pub const fn status(&self) -> u16 {
        self.status
    }

    #[must_use]
    pub fn content_type(&self) -> Option<&[u8]> {
        self.content_type.as_deref()
    }

    #[must_use]
    pub const fn body(&self) -> &Bytes {
        &self.body
    }
}

impl fmt::Debug for AccountProbeUpstreamResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccountProbeUpstreamResponse")
            .field("status", &self.status)
            .field(
                "content_type",
                &self.content_type.as_ref().map(|_| "<present>"),
            )
            .field("body", &"<redacted>")
            .finish()
    }
}

/// 连接测试失败发生的稳定责任边界
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountProbeErrorSource {
    Gateway,
    Provider,
    Upstream,
}

impl AccountProbeErrorSource {
    /// 返回管理端 wire 使用的稳定值
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Provider => "provider",
            Self::Upstream => "upstream",
        }
    }
}

/// 管理端连接测试的终态错误
///
/// `gateway` 保留稳定分类，`upstream_response` 只面向本次认证管理请求展示源响应
#[derive(Debug)]
pub struct AccountProbeError {
    gateway: GatewayError,
    source: AccountProbeErrorSource,
    send_state: Option<UpstreamSendState>,
    upstream_response: Option<AccountProbeUpstreamResponse>,
}

impl AccountProbeError {
    #[must_use]
    pub const fn new(
        gateway: GatewayError,
        source: AccountProbeErrorSource,
        send_state: Option<UpstreamSendState>,
        upstream_response: Option<AccountProbeUpstreamResponse>,
    ) -> Self {
        Self {
            gateway,
            source,
            send_state,
            upstream_response,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> GatewayErrorKind {
        self.gateway.kind()
    }

    #[must_use]
    pub const fn source(&self) -> AccountProbeErrorSource {
        self.source
    }

    #[must_use]
    pub const fn send_state(&self) -> Option<UpstreamSendState> {
        self.send_state
    }

    #[must_use]
    pub fn client_message(&self) -> &str {
        self.gateway.client_message()
    }

    #[must_use]
    pub fn client_error_type(&self) -> Option<&str> {
        self.gateway.client_error_type()
    }

    #[must_use]
    pub fn client_error_code(&self) -> Option<&str> {
        self.gateway.client_error_code()
    }

    #[must_use]
    pub const fn upstream_response(&self) -> Option<&AccountProbeUpstreamResponse> {
        self.upstream_response.as_ref()
    }
}

impl From<GatewayError> for AccountProbeError {
    fn from(gateway: GatewayError) -> Self {
        Self::new(gateway, AccountProbeErrorSource::Gateway, None, None)
    }
}

pub trait AccountProbe: Send + Sync {
    /// 插件管理操作传入其冻结快照；其他调用可使用当前发布视图
    fn probe(
        &self,
        request: AccountProbeRequest,
        snapshot: Option<std::sync::Arc<crate::routing::RuntimeSnapshot>>,
    ) -> BoxFuture<'_, Result<AccountProbeResult, AccountProbeError>>;
}

//! 固定官方端点的 HTTP 传输与取消边界，允许注入测试传输但不继承 API 账号环境
//! Copyright 2026 OpenAI，Rust 修改适用随附非商业许可证

use crate::{
    CancellationToken, ChatGPTError,
    errors::{api_error, cancelled, network_error},
};
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde_json::Value;
use std::{collections::BTreeMap, pin::Pin, time::Duration};

pub type ResponseBody = Pin<Box<dyn Stream<Item = Result<Bytes, ChatGPTError>> + Send>>;

pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: ResponseBody,
}

/// 注入实现必须拒绝重定向、不重试、不记录正文或凭据
#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ChatGPTError>;
}

pub struct ReqwestTransport(reqwest::Client);

impl ReqwestTransport {
    /// 宿主客户端须禁用重定向和自动重试，并按自身策略配置可信 CA 与账号出口
    pub fn from_client(client: reqwest::Client) -> Self {
        Self(client)
    }

    pub fn new() -> Result<Self, ChatGPTError> {
        Self::with_proxy(None)
    }
    pub fn with_proxy(proxy: Option<&str>) -> Result<Self, ChatGPTError> {
        let mut builder = reqwest::Client::builder()
            .use_rustls_tls()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy();
        if let Some(proxy) = proxy {
            builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| network_error())?);
        }
        builder.build().map(Self).map_err(|_| network_error())
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ChatGPTError> {
        let method =
            reqwest::Method::from_bytes(request.method.as_bytes()).map_err(|_| network_error())?;
        let mut builder = self.0.request(method, &request.url);
        for (key, value) in request.headers {
            builder = builder.header(key, value);
        }
        let response = builder
            .body(request.body)
            .send()
            .await
            .map_err(|_| network_error())?;
        // reqwest 的 none 返回 3xx，而 Fetch 的 error 会拒绝整次请求
        if response.status().is_redirection() {
            return Err(network_error());
        }
        let headers = response
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_owned())))
            .collect();
        Ok(HttpResponse {
            status: response.status().as_u16(),
            headers,
            body: Box::pin(
                response
                    .bytes_stream()
                    .map(|result| result.map_err(|_| network_error())),
            ),
        })
    }
}

pub(crate) async fn send(
    transport: &dyn Transport,
    request: HttpRequest,
    signal: &CancellationToken,
    timeout: Duration,
) -> Result<HttpResponse, ChatGPTError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut response = tokio::select! {
        biased;
        _ = signal.cancelled() => Err(cancelled()),
        result = tokio::time::timeout(timeout, transport.send(request)) => result.unwrap_or_else(|_| Err(network_error())),
    }?;
    let signal = signal.clone();
    response.body = Box::pin(futures_util::stream::unfold(
        (response.body, false),
        move |(mut body, done)| {
            let signal = signal.clone();
            async move {
                if done {
                    return None;
                }
                let item = tokio::select! {biased;_=signal.cancelled()=>Some(Err(cancelled())),_=tokio::time::sleep_until(deadline)=>Some(Err(cancelled())),item=body.next()=>item};
                let done = item.as_ref().is_some_and(Result::is_err);
                item.map(|item| (item, (body, done)))
            }
        },
    ));
    Ok(response)
}

pub(crate) async fn json_response(
    mut response: HttpResponse,
    signal: &CancellationToken,
    deadline: Duration,
) -> Result<Value, ChatGPTError> {
    let status = response.status;
    let request_id = response.headers.get("x-request-id").cloned();
    let invalid = || {
        if !(200..300).contains(&status) {
            api_error(None, Some(status), request_id.as_deref())
        } else {
            ChatGPTError::new(
                "invalid_response",
                "ChatGPT returned an unexpected response. Try again.",
                true,
            )
            .status(status)
        }
    };
    let read = async {
        let mut bytes = Vec::new();
        while let Some(chunk) = response.body.next().await {
            let chunk = chunk.map_err(|error| {
                if error.code == "cancelled" {
                    error
                } else {
                    invalid()
                }
            })?;
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| invalid())
    };
    tokio::select! {
        biased;
        _ = signal.cancelled() => Err(cancelled()),
        result = tokio::time::timeout(deadline, read) => result.unwrap_or_else(|_| Err(cancelled())),
    }
}

pub(crate) fn request(method: &'static str, url: &str) -> HttpRequest {
    HttpRequest {
        method,
        url: url.into(),
        headers: BTreeMap::new(),
        body: Vec::new(),
    }
}

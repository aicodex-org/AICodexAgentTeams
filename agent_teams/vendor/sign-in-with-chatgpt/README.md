# Sign in with ChatGPT Rust SDK

OpenAI 官方 [@siwc/local](https://github.com/openai/sign-in-with-chatgpt-devkit/tree/0a36fefeb913055c8c7a1a29b63d82b96b2e841a/packages/local) 的非官方 Rust 移植，独立 Cargo 库，不依赖 Node、React 或 AICodexAgentTeams

支持动态 OAuth 注册、IPv4 loopback + PKCE 登录、多账号切换、加密存储、刷新轮换恢复、模型目录与流式 Responses

## 使用

Rust 1.88 或以上，Tokio runtime

```toml
[dependencies]
sign-in-with-chatgpt = { path = "../sign-in-with-chatgpt-rust" }
```

宿主在可信本地进程提供 CredentialEncryption 的 OS 加密实现，构造一个共享客户端

```rust
use std::{path::PathBuf, sync::Arc};
use sign_in_with_chatgpt::{ChatGPT, ChatGPTConfig, ChatGPTError, CredentialEncryption};

fn create_client(
    encryption: Arc<dyn CredentialEncryption>,
    private_directory: PathBuf,
) -> Result<ChatGPT, ChatGPTError> {
    ChatGPT::new(ChatGPTConfig {
        app_name: "My App".into(),
        app_id: "my-app".into(),
        redirect_port: 0,
        storage_dir: Some(private_directory),
        credential_encryption: encryption,
        open_browser: None,
        send_host_id: false,
    })
}
```

storage_dir 必须是绝对路径；不提供时使用用户目录下的 .config/<app_id>，端口 0 选择可用 IPv4 回环端口

CredentialEncryption 有四个方法：id、is_available、encrypt、decrypt，后三者异步执行。实现应调用 Keychain、Windows OS 加密或兼容安全存储，不可使用明文、硬编码密钥或测试加密提供者。id 是稳定格式标识，已有文件必须用原加密提供者解密；密钥不可用时 is_available 返回 false

## 公共方法

- sign_in(SignInOptions)：显式用户操作触发浏览器登录，可选择新账号、已有账号或明确重授权分享权限
- cancel_sign_in()：取消当前登录
- get_session()：返回不含凭据的会话快照，存储失败呈现为 reauth_required 和错误
- list_profiles()、select_profile(id)：列出和选择账号，未完成注册呈现为 pending profile
- list_models(CancellationToken)：使用选定账号的 OAuth token 请求官方模型目录
- stream_response(StreamResponseOptions)：文本或类型化消息输入，返回 ResponseText，on_delta 提供增量回调
- subscribe(listener)：立即推送当前快照并在每次发布时调用，保留返回的 Subscription；丢弃句柄取消订阅
- disconnect()：移除本地令牌，保留注册与已验证身份；远程撤销失败通过 revocation_failed 呈现

ChatGPT 可以 clone，多份 handle 共享同一个状态机。回调应快速返回，界面只接收 SessionState / LoginProfile / ChatGPTError，不暴露本地加密提供者或传输接口

请求用 CancellationToken 取消；调用方取消后，已开始的刷新会继续完成加密轮换，不会因取消丢失唯一有效 successor。宿主关闭 runtime 前应让正在持久化的操作结束

MessageRole 只允许 user、assistant、developer，系统指导通过 instructions 提供。SDK 固定请求 https://api.openai.com/v1，不继承 OPENAI_API_KEY、组织、项目、自定义 OpenAI 请求头或调试日志设置，不跟随重定向，也不自动重试 Responses

## 验证

默认测试不需要 Node、OpenAI 账号或网络服务，只使用合成身份、临时认证加密和本机回环 HTTP

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

官方对照测试需要固定提交的 devkit checkout、Node 22.12+ 及已构建的 packages/local/dist，不会访问真实账号

```sh
cd ../sign-in-with-chatgpt-devkit
npm ci --ignore-scripts
npm run build -w @siwc/local
cd ../sign-in-with-chatgpt-rust
SIWC_UPSTREAM_DIR=../sign-in-with-chatgpt-devkit cargo test --all-features --locked
```

[行为与测试映射](docs/parity.md)说明移植范围、跨语言对照和平台验收边界

## 许可证和接入边界

本库属于官方源码的 Modified Work，保留 [Sign-in with ChatGPT DevKit Noncommercial License v1.0](LICENSE)；不是 MIT 或 Apache 许可，商业使用需要 OpenAI 单独书面授权，许可证不授予服务访问或商标权

源码、测试和锁文件适用于固定官方提交，不代表官方背书，也不代表真实账号、生产系统或全平台验收。OS 加密适配、应用 UI、真实账号登录及宿主接入由使用方独立完成；React 组件、品牌资产及 Paste Perfect 原生示例不在这个 Rust 本地 SDK 中

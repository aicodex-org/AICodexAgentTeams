# 行为与测试映射

## 基线与范围

官方来源：https://github.com/openai/sign-in-with-chatgpt-devkit

提交：0a36fefeb913055c8c7a1a29b63d82b96b2e841a，packages/local 版本 0.1.0

Rust 库覆盖 index.ts、oauth.ts、storage.ts、models.ts、responses.ts、errors.ts 和 types.ts 的本地 SDK 职责，使用原官方 OAuth scope、资源、端点、注册合同和错误分类

官方 React、品牌字体和图标、组件画廊、Paste Perfect 的 Electron/Swift、原生粘贴与应用打包测试属于 UI 和示例应用，不是本地 SDK 合同

## 合同

- 首次注册使用 dynamic_agent_client，保存返回的 client ID 后才兑换一次性 code；invalid_grant 只重新授权一次，不重复注册
- 回调仅接受 127.0.0.1 的 /auth/callback、正确 Host、GET、唯一且恒时比较的 state，拒绝重复 code、client ID 或账号注册不匹配
- 身份要求 RS256、官方 issuer、对应 audience、exp、iat、非空 sub，以及授权 nonce 与 azp 绑定；保存账号按已验证 subject 比较，不以邮箱决定身份
- 版本 1/2 凭据迁移为版本 3 加密 envelope；版本 1 注册保留旧回调边界，要求新增注册而不是改变 callback host
- 读写持有进程间目录锁，120 秒 stale、20 秒 heartbeat、100 毫秒重试、45 秒等待上限；原子写入和同步，清理临时文件，不覆盖不可恢复或不可解密的文件
- Unix 文件拒绝 symlink、非当前用户所有权及非 owner-only 权限，目录与凭据文件分别为 0700、0600
- 刷新前判断 60 秒到期窗口和 earliestRefreshAt；返回身份时先保存加密 pending checkpoint，再验证 JWKS，故障时保留旧凭据和 successor
- pending ID token 按可信本地接收时间验证，重启后不重新提交旧 refresh grant；验证失败清除全部令牌，恢复后的过期 access token 不发送到 API
- 分享权限和模型目录以服务端为准，无本地默认模型或权限回退；HTTP 401 本身不证明刷新授权已撤销
- 退出仅撤销 refresh token，优先使用 pending successor；网络或 5xx 撤销最多尝试两次，失败仍移除本地令牌并保留注册
- Responses 包含 store:false、stream:true，只在 response.completed 成功，处理 UTF-8 分块、CR/LF、错误、incomplete、提前 EOF 与取消
- 错误保留官方 machine code、retryable 和安全诊断，不泄漏服务端消息、输入、凭据或加密提供者错误详情

## 测试对应

Rust 使用分组、表驱动和编译期类型合同，不以函数数量和 Node 的测试数量作一一对应

| 官方用例 | Rust 对应 | 主要覆盖 |
| --- | --- | --- |
| authorization.test.mjs，4 项 | tests/authorization.rs | opener 参数、无 token URL、subject/nonce 绑定 |
| errors.test.mjs，10 项 | tests/errors.rs | 成功与错误 JSON body 取消、模型取消、畸形 JSON |
| refresh.test.mjs，12 项 | tests/refresh.rs、tests/restart.rs | 七类 JWKS 故障、五类无效身份、无 ID token 轮换、退出、expired successor、新进程恢复 |
| responses.test.mjs，11 项 | tests/responses.rs | 直连请求、消息投影、六类 SSE 失败、取消、错误响应与无重试 |
| storage.test.mjs，20 项 | tests/storage.rs | 加密、两种迁移、异步接口、配置、损坏保护、权限、symlink 和锁 |

补充回归覆盖：多账号切换、注册复用、订阅生命周期、回调安全头、模型目录拒绝与调用方取消后刷新持久化

upstream-parity feature 校验官方提交，运行官方 57 项本地测试，并对 88 个合成错误、模型和 SSE 场景比较两种实现的完整 JSON 输出；加密 state 与 host identity 经官方 Node store 双向读写，验证 envelope 和 provider 合同

## Rust 类型适配

公开方法使用 snake_case；会话、错误及模型的 serde JSON 保留 camelCase 字段与官方 status 值，凭据类型不公开

redirect_port:u16、必需的 Arc<dyn CredentialEncryption>、消息角色枚举及字符串字段在编译期排除 JavaScript 无效输入；加密方法统一异步，同步 OS 实现可直接返回异步结果。Subscription 的 Drop 代替 JavaScript unsubscribe 函数

传输和 OIDC 缓存由一个 SDK 实例拥有，多份 clone 共享状态；不同实例和进程通过官方同名目录锁串行存储操作。测试加密仅用于合成数据，生产 OS 提供者的格式与密钥由宿主拥有

`hosted::OAuthClient` 由宿主管理回调和凭据存储，使用[官方 discovery](https://auth.openai.com/.well-known/openid-configuration) 公布的固定浏览器入口在本地生成授权链接，不要求生成阶段联网；令牌交换、刷新和身份签名验证仍使用实时 discovery 与 JWKS

## 验收边界

默认测试不连接官方服务，对照测试只运行官方 mock 套件，不测试真实注册、分享资格、账单或实际账户权限

macOS/Linux/Windows 的默认 opener 与 OS 加密能力需要分别验证；本机源码与 mock 验证不能替代跨平台运行证据、商业授权或宿主真实登录验收

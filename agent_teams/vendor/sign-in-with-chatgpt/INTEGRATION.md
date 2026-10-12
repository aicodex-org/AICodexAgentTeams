# 网关 SDK 接入

本目录基于 `sign-in-with-chatgpt-rust` 源码快照，`SOURCE.json` 保存导入时的基准文件摘要，不代表宿主适配后的文件摘要。官方 DevKit 基准提交为 `0a36fefeb913055c8c7a1a29b63d82b96b2e841a`

`hosted::OAuthClient` 供持有凭据存储的宿主调用，复用 SDK 的发现、换票、JWT/JWKS 校验与刷新轮换。宿主拥有管理员身份绑定、授权有效期、独占领取、凭据持久化与提交事务，浏览器只得到授权 URL 和安全状态。网关沿用现有明文 PostgreSQL 凭据 JSON 与 Redis 授权事务存储，未启用 SDK 的本地文件加密；数据库、Redis 与备份按敏感数据保护

网关使用固定回环回调 URL，让用户粘贴浏览器地址栏中的完整回调，SDK 不在服务端打开浏览器或启动本地文件账号库。账号安装 UUID 随凭据保存，并在重新授权时复用。HTTP 传输复用网关账号客户端的代理、自定义 CA 与连接配置，不跟随重定向或继承隐式系统代理

刷新结果先回调宿主保存 successor checkpoint，无新 ID token 的刷新也保存 checkpoint。完成或恢复验证后才交付可执行凭据，恢复 checkpoint 不重复提交旧 refresh token。待恢复记录立即进入刷新候选，最终验证状态与凭据在同一事务提交。账号内部身份使用 `chatgpt:` 命名空间，原始 OIDC subject 保存在 SDK 记录中

本目录保留独立的 [LICENSE](LICENSE) 和 [NOTICE](NOTICE)，不随宿主适用 Apache-2.0。这是官方 TypeScript DevKit 的非官方 Rust 移植，具体许可按随附文件执行

验证入口：

```sh
cargo test --manifest-path agent_teams/vendor/sign-in-with-chatgpt/Cargo.toml --locked
```

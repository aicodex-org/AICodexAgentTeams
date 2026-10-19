<!-- prettier-ignore -->
<div align="center">

<img src="webui/public/favicon.svg" alt="AICodexAgentTeams" width="80" height="80" />

# AICodexAgentTeams

面向 Codex 的自托管多账号 AI 网关

[![Release](https://img.shields.io/github/v/release/aicodex-org/AICodexAgentTeams?display_name=tag&sort=semver&style=flat-square)](https://github.com/aicodex-org/AICodexAgentTeams/releases)
[![License: Apache 2.0](https://img.shields.io/badge/License-Apache%202.0-blue.svg?style=flat-square)](LICENSE)

[快速预览](#快速预览) · [快速开始](#快速开始) · [客户端接入](#客户端接入) · [文档](#文档) · [社区](#社区) · [许可证](#许可证)

</div>

> [!NOTE]
> 本项目提供 Responses API，不支持 `/v1/chat/completions`，接入前请确认客户端支持 Responses 协议

## 快速预览

本项目未配置公共预览服务，可按[源码构建](deploy/README.md#镜像升级与源码构建)在本地查看管理端的系统概览、账号分组、代理管理与用量统计

## 快速开始

使用 Docker Compose 部署，同时启动 PostgreSQL 和 Redis

安装脚本依赖本仓库已经发布的 Release 附件和镜像，未发布时请使用[源码构建](deploy/README.md#镜像升级与源码构建)

以下命令适用于 Linux amd64/arm64，需要 Docker Engine、Docker Compose Plugin、curl 和 OpenSSL

已有部署请先看 [升级说明](deploy/README.md#镜像升级与源码构建)，不要覆盖原配置

### 一键安装

请确保当前用户能访问 Docker，并可通过 `sudo` 或 root 设置目录权限

```bash
curl -fsSL https://raw.githubusercontent.com/aicodex-org/AICodexAgentTeams/main/deploy/install.sh -o install.sh && bash install.sh
```

[安装脚本](deploy/install.sh) 默认安装到当前目录下的 `AICodexAgentTeams/`，下载同一正式 Release 的部署文件，
自动生成密码、设置目录权限并启动服务

完成后保存显示的访问地址和管理员密码，再[登录管理端](#登录管理端)并[添加账号与客户端密钥](#添加账号与客户端密钥)

可在执行 `bash install.sh` 时传入环境变量：

| 变量 | 用途 | 默认值 |
| --- | --- | --- |
| `INSTALL_DIR` | 安装目录，建议使用绝对路径 | 当前目录下的 `AICodexAgentTeams/` |
| `AICODEX_AGENT_TEAMS_RELEASE_TAG` | 指定发布标签 | 最新正式版本 |
| `ADMIN_PASSWORD` | 管理员初始密码，至少 12 位，不能包含 `$`，不能使用常见弱口令 | 随机生成 |

例如，自定义安装目录：

```bash
INSTALL_DIR="$HOME/services/AICodexAgentTeams" bash install.sh
```

重复运行时请使用同一安装目录，检测到 `deploy/config.yaml` 后，脚本保留现有配置和部署文件，
忽略传入的管理员密码，也不执行版本升级

### 手动安装

自行下载部署文件、配置密码和启动服务的完整步骤见 [部署文档](deploy/README.md#手动安装)

### 登录管理端

部署完成后，打开 `http://127.0.0.1:9699`，使用 `admin@aicodexagentteams.local` 和管理员密码登录

API Key 持有者可在登录页切换身份，查看自己的用量与额度，并通过「密钥配置」复制 Codex 配置或导入 CCSwitch

默认地址只能在服务器本机访问，从其他设备使用时，需要配置
[HTTPS 反向代理](deploy/README.md#公网访问)

### 添加账号与客户端密钥

1. 在「账号」中添加账号，完成授权或导入
2. 按需建立账号分组，再创建客户端密钥并选择可用分组，**不选分组表示可使用全部账号**
3. 打开密钥的「使用密钥」，复制客户端配置

## 客户端接入

**Codex CLI / 桌面端**：在「使用密钥」中按操作系统复制配置，或通过 CCSwitch 导入，合并配置后重启 Codex

完整步骤、生图配置与排障见[客户端配置](deploy/README.md#客户端配置)

**其他 Responses API 客户端**：填写以下信息：

| 配置 | 值 |
| --- | --- |
| Base URL | `http://127.0.0.1:9699/v1`；远程接入使用服务器的 HTTPS 地址 |
| API Key | 管理端创建的客户端密钥 |

查询该密钥可见的模型目录：

```bash
curl http://127.0.0.1:9699/v1/models \
  -H 'Authorization: Bearer <client-api-key>'
```

## 文档

| 任务 | 文档 |
| --- | --- |
| 部署与使用 | [部署、备份与恢复](deploy/README.md) · [客户端接入与生图](deploy/README.md#客户端配置) |
| 接口集成 | [API 参考](docs/api.md) · [模型定价](docs/api.md#模型定价) |
| 使用插件 | [安装、配置与使用](docs/plugins.md) |
| 开发插件 | [SDK 与合同](agent_teams/crates/gateway-plugin/sdk/README.md) · [打包工具](agent_teams/apps/plugin-cli/README.md) |
| 开发宿主 | [开发与源码联调](docs/development.md) · [系统架构](docs/architecture.md) · [管理端主题](docs/theme.md) · [数据库迁移](agent_teams/migrations/README.md) |
| 参与协作 | [贡献、审查与验证](CONTRIBUTING.md) |

## 社区

欢迎到 [Discussions 讨论区](https://github.com/aicodex-org/AICodexAgentTeams/discussions)交流：
[使用问答](https://github.com/aicodex-org/AICodexAgentTeams/discussions/categories/使用问答)、
[想法讨论](https://github.com/aicodex-org/AICodexAgentTeams/discussions/categories/想法讨论)、
[实验反馈](https://github.com/aicodex-org/AICodexAgentTeams/discussions/categories/实验反馈)与
[经验分享](https://github.com/aicodex-org/AICodexAgentTeams/discussions/categories/经验分享)

感谢 [LINUX DO](https://linux.do) 社区提供开放、友善的技术交流平台

## 许可证

本项目基于 [Apache License 2.0](LICENSE) 开源

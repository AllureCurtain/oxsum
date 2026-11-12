# oxsum

[![CI](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml/badge.svg)](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

[English](README.md)

**面向 AI API 的自托管、可核验计费：调用前冻结额度，按钉住的销售条款结算记录下的用量；复式记账，客户可以自行检查 Merkle 证明。Rust + PostgreSQL，OpenAI 兼容网关。**

oxsum 是 AI 服务商在自己的基础设施上运行的按量计费系统。一个 Business 拥有供应商、客户销售条款和收款；个人与团队客户账户拥有自己的余额、API 密钥和账单。运营者使用内置门户 `/app`。应用和 Agent SaaS 开发者可以把网关和带范围的计量 API 嵌进自己的产品。Rust 服务端和 React 界面打进同一个二进制；数据在 PostgreSQL 17。这种部署不向 oxsum 平台付费。

一次被接受的模型调用会先冻结适用的上限，再按钉住的条款结算记录下的用量，并释放未使用的额度。购买资金、促销额度、套餐配额和已批准的信用额度各自记账。每笔结算追加进 Merkle 透明日志。客户若保存了条目哈希——更强的检查还需要自己留存的较早日志头，以及自己选择信任的签名公钥——就可以在 `/verify` 或 [`verify-oxsum-bill`](web/verify-sdk) 上核验该账单。检查范围见 [docs/proofs.md](docs/proofs.md)。它不能证明上游供应商报告的 token 数是真实的。

**状态。** 工作区版本是 0.1.0，还没有 GitHub Release。运营者上手仍有缺口，跟踪在 [#725](https://github.com/AllureCurtain/oxsum/issues/725)。`verify-oxsum-bill` 尚未发布到 npm（2026-11-12 的 `npm view verify-oxsum-bill` 返回 404）；请从 [`web/verify-sdk`](web/verify-sdk) 构建。同一天访问 [oxsum.785777.xyz](https://oxsum.785777.xyz) 会重定向到 `/app` 并返回内嵌应用的 HTML；`GET /api/v1/version` 没有返回版本文档。这次探测没有登录，也没有确认部署的提交、余额、凭据或供应商连通性。拉取请求上变绿的 CI 只是快速层，不是完整的候选发布检查（[候选门槛](docs/self-hosted-release.md)）。

本页是 [README.md](README.md) 的中文说明，产品文档仍以英文为准。

## 在本地运行

需要不低于工作区 MSRV 1.98 的 Rust stable，以及带 Compose 的 Docker。`.env.example` 里的 `DATABASE_URL` 已经指向 `compose.yaml` 创建的数据库。存在 `.env` 时，服务启动会加载它。

```bash
git clone https://github.com/AllureCurtain/oxsum.git
cd oxsum
cp .env.example .env
docker compose up -d
cargo run -p oxsum-server
```

第一次 `cargo run` 会编译整个工作区。然后查看 [健康检查](http://127.0.0.1:3000/healthz) 和 [就绪检查](http://127.0.0.1:3000/readyz)。就绪检查只在 PostgreSQL 可达且迁移已应用时返回成功。

打开门户需要运营者令牌。在 `.env` 中取消 `OXSUM_ADMIN_TOKEN` 的注释，设为 `openssl rand -hex 24` 的输出（至少 16 个字符），然后重启。按[安装手册](docs/self-hosted-install.md)做一次性所有者设置：签发 setup token，在 [http://127.0.0.1:3000/app/setup](http://127.0.0.1:3000/app/setup) 接受，再到 [http://127.0.0.1:3000/app](http://127.0.0.1:3000/app) 登录并选择 Business。本地检出用 `.env` 和 `cargo run`。镜像部署用 [`deploy/`](deploy/docker-compose.core.yml) 和同一份手册。

存入供应商或支付凭据之前需要 `OXSUM_SECRET_KEY`（`openssl rand -base64 32`）。`OXSUM_HEAD_SIGNING_KEY` 可选；不设置时签名日志路由返回 503，钱包本身仍可服务。

客户注册在所有者显式开放之前保持仅邀请。`OXSUM_SIGNUP` 不能覆盖已保存的策略。注册不发放购买资金，也不授予员工权限。进程在跑、配了渠道或商户，都不等于有过一次成功的模型调用或支付。本修订没有浏览器编辑器的地方，使用文档中的设置 API（[#725](https://github.com/AllureCurtain/oxsum/issues/725)）。部署令牌留在运营工具里。

`docker build -t oxsum .` 构建包含二进制、React 应用和验证器资源的本地镜像。镜像仍需要外部 PostgreSQL 和部署配置。`.env` 不会打进镜像。支持的安装路径是[手册](docs/self-hosted-install.md)，它固定不可变的镜像引用。

## 一份证明能确立什么

留存的内容哈希让 `verify_bundle` 检查条目字节是否仍哈希到该值，以及包含证明是否到达所给的日志头。对验证器认识的结算描述，另一次金额检查会按条目内的用量和费率重算记录下的金额。签名的价格绑定和销售条款绑定、相对读者早先归档的日志头的一致性证明、用量报告或证据包检查，以及见证方联合签名，各自回答不同的问题。安装了 WASM 资源时，`/verify` 在浏览器里跑检查；没有这些资源时，退回服务端渲染的表单。

同一次检查不能证明供应商确实返回了那么多 token，不能证明和证明包一起送来的公钥是独立可信的运营者身份，不能证明见证方在组织上独立，也不能证明一份未锚定的报告列出了每一笔结算。配置了 `OXSUM_WITNESS_URLS` 时，另一个 oxsum 进程可以为日志头联合签名；这演示的是协议。读者自己保存的日志头互相比较，可以发现分叉。完整对照表在 [docs/proofs.md](docs/proofs.md)。客户步骤在 [docs/verify-your-bill.md](docs/verify-your-bill.md)。线格式在 [docs/transparency-log-v1.md](docs/transparency-log-v1.md)。

`verify-oxsum-bill`（尚未上 npm）把同一套检查提供给 Node 和浏览器，包括 `<verify-bill>`、`<verify-statement>` 和 `<verify-usage-report>`。从 [`web/verify-sdk`](web/verify-sdk) 构建，或用本地打包的 tarball 运行 [`examples/verify-consumer`](examples/verify-consumer)：

```bash
bash scripts/check-verify-consumer.sh
```

## 能力

- **LLM 按量计费。** 冻结上限，结算记录下的用量，释放未使用的额度。未知用量、估算和封顶结算都是显式结果。金额是最小货币单位的整数（1 credit = 1,000,000）。
- **OpenAI 兼容网关。** `POST /v1/chat/completions` 和 `POST /v1/responses`，以及 [docs/integrations.md](docs/integrations.md) 记录的 Anthropic Messages、Gemini、embeddings 和 rerank。把官方 SDK 的基址指到本部署的 `/v1`。
- **复式记账和 Merkle 日志。** 记账引擎是 [doubleentry](https://github.com/hupe1980/doubleentry)（MIT OR Apache-2.0），放在 `crates/doubleentry`。oxsum 在其上做钱包、销售条款和证明包。
- **Business 门户和客户账户。** React 应用在 `/app` 提供英文和中文，发布构建会把它嵌进二进制。旧的 `/dashboard`、认证和 `/admin` 路径会重定向过去。`/verify` 保持服务端渲染。安装了 WASM 资源时，浏览器运行随该服务器发布的验证器；否则表单在服务器上检查。
- **收款通道。** Stripe、Airwallex 和 Hyperswitch 结账、兑换码，以及员工记录的收据。`POST /api/v1/topups` 拒绝客户自行入账。结账响应不是支付成功；签名的供应商事件和按用途的履约才确立订单入了什么账。[通道矩阵](docs/rails.md) 把本地夹具和供应商、真实资金证据分开。
- **可嵌入的计量。** 所有者签发的、带明确范围的凭据，可以为没有经过网关的事件做冻结和结算。嵌入式应用的完整生命周期属于 [#725](https://github.com/AllureCurtain/oxsum/issues/725)；本仓库里已有的某个范围 API，并不表示每一个计划中的端点都已通过验收。

## 架构

```
LLM 客户端和其他 API 调用方
        │
        ▼
一个 Rust 进程（axum）
  /v1      网关转发
  /api/v1  计费、Business 和客户 API
  /app     React 门户（发布构建嵌进二进制）
  /verify  服务端渲染页面；安装了资源时在浏览器做 WASM 检查
        │
        ▼
oxsum-core  ──转发──►  上游模型供应商
        │
        ▼
doubleentry     复式记账和 Merkle 日志
        │
        ▼
PostgreSQL 17   每个组织一个账本 schema，另加共享的 oxsum 表
```

领域逻辑在 `crates/core`。网关负责协议和编排。`crates/verify` 是纯检查器，与浏览器共用，并保持 wasm32 干净。更多说明在 [docs/architecture.md](docs/architecture.md)。

## 隔离演示

`demo/demo.py` 是当前维护的本地流程。它创建自己的一次性 PostgreSQL 数据库，创建失败就退出。它不读取检出目录的 `.env`，不把应用数据放进用于建库的那个数据库，也不联系付费供应商或支付服务。一次运行包括：部署授权的所有者同意、受邀客户、一笔合成的人工收据、显式的客户销售条款、通过官方 OpenAI Python SDK 的一次流式桩响应，以及在服务停止、其数据库删除之后，用打包的验证器做离线检查。

前提：Python 3.10+、Rust、Node 20.16+、npm、具有 `CREATEDB` 角色的 PostgreSQL 17、`wasm32-unknown-unknown` 目标，以及匹配的 `wasm-bindgen` CLI（[开发环境要求](docs/development.md#requirements)）。该命令会构建服务端和验证器，并尊重 Cargo 配置的目标目录。

```bash
python3 -m venv /tmp/oxsum-demo-venv
/tmp/oxsum-demo-venv/bin/pip install -r demo/requirements.txt
docker compose up -d
/tmp/oxsum-demo-venv/bin/python demo/demo.py
```

建库连接默认是 `postgres://postgres:postgres@127.0.0.1:5432/postgres`。另一个本地集群请在进程环境里设置 `DATABASE_URL`；它只用于创建和删除这一次的数据库。应用只拿到新的数据库 URL 和生成的密钥，继承来的 `OXSUM_*` 配置会被去掉。

确定性的这一轮报告 23 个输入 token 和 11 个输出 token，每个 token 一个最小单位。费用恰好是 34 个最小单位，可用余额是 99,999,966，没有剩余冻结。留存的哈希、调用前的日志头和公钥、签名的供应与销售绑定、一致性证明和新的日志头会写进证据目录（`--output` 指定一个新目录）。公钥来自这个本地夹具，不是独立见证过的运营者身份。

账单、签名和历史检查必须通过。改过的内容、哈希、公钥和归档日志头必须失败。这是本地桩上的一笔即时条目，不是已关闭账期的报告、独立见证、真实支付或真实供应商认证。

议题 [#728](https://github.com/AllureCurtain/oxsum/issues/728) 替换了另一套注册和充值假设下的旧脚本。本树里的脚本就是那次替换。请用本文件的说明；不要把更早的记录当作后续提交的验收证据。

## 更早的界面图片

这些截图来自 2026-10-24（[#362](https://github.com/AllureCurtain/oxsum/issues/362)）：从充值到 Requests、Bills 和 `/verify` 的一组真实页面。侧栏是更早的扁平导航。当前门户分成 Main、Configure 和 Account，并增加了 Orders、Plans、Billing、Evidence 和 Business 等页面。`demo.gif` 的开头一帧和下面的静图对过那套旧界面。动画后面的帧没有逐帧重放。它们说明的是那个修订，不是当前候选版本的验收。

![更早的界面：充值、请求、账单和核验](docs/assets/demo.gif)

| 入账 | 证据 |
| --- | --- |
| ![更早的 Hyperswitch 充值页](docs/assets/topup-hyperswitch.png) | ![更早的账单页](docs/assets/bills.png) |

## 文档

- [产品概览](docs/product-page.md)、[行为](docs/product.md) 和 [用户指南](docs/user-guide.md)
- [API](docs/api.md)、[集成](docs/integrations.md) 和 [拒绝的操作](docs/unsupported.md)
- [核验账单](docs/verify-your-bill.md)、[证明范围](docs/proofs.md) 和 [支付证据](docs/rails.md)
- [安装](docs/self-hosted-install.md)、[升级与恢复](docs/self-hosted-upgrade.md) 和 [候选与发布门槛](docs/self-hosted-release.md)
- [开发](docs/development.md)、[架构](docs/architecture.md) 和 [决策](docs/decisions.md)
- [安全](SECURITY.md) 和 [支持](SUPPORT.md)

## 它处在哪一层

oxsum 是运营者跑在自己的模型供应商旁边的计费系统。项目决策把网关描述为与 LiteLLM、new-api 这类自托管代理有重叠，并把目标描述为：结算落在账本上，客户可以在 [docs/proofs.md](docs/proofs.md) 的范围内检查，而不是一份尽力而为的消费日志。这些记录没有说 oxsum 取代那些代理。供应商沙箱认证、参考容量、独立试点和对外发布，与本地夹具是不同的门槛（[通道矩阵](docs/rails.md)，[发布清单](docs/roadmap/self-hosted-v1/RELEASE-CHECKLIST.md)）。

## 许可证

MIT OR Apache-2.0。文本是 [LICENSE-MIT](LICENSE-MIT) 和 [LICENSE-APACHE](LICENSE-APACHE)。工作区清单使用同一条 SPDX 表达式。`crates/doubleentry` 保留原作者的版权声明；见该目录中的 LICENSE 文件。

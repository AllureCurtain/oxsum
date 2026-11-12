# oxsum

[![CI](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml/badge.svg)](https://github.com/AllureCurtain/oxsum/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](README.md#license)

[English](README.md)

**面向 AI API 的自托管、可核验计费：调用前冻结额度，按钉住的销售条款结算记录下的用量；复式记账，客户可以自行检查 Merkle 证明。Rust + PostgreSQL，OpenAI 兼容网关。**

oxsum 是 AI 服务商在自己的基础设施上运行的按量计费系统。一个 Business 拥有供应商、客户销售条款和收款；个人与团队客户账户拥有自己的余额、API 密钥和账单。运营者使用内置门户 `/app`。应用和 Agent SaaS 开发者可以把网关和带范围的计量 API 嵌进自己的产品。Rust 服务端和 React 界面打进同一个二进制；数据在 PostgreSQL 17。这种部署不向 oxsum 平台付费。

一次被接受的模型调用会先冻结适用的上限，再按钉住的条款结算记录下的用量，并释放未使用的额度。购买资金、促销额度、套餐配额和已批准的信用额度各自记账。每笔结算追加进 Merkle 透明日志。客户若保存了条目哈希——更强的检查还需要自己留存的较早日志头，以及自己选择信任的签名公钥——就可以在 `/verify` 或 [`verify-oxsum-bill`](web/verify-sdk) 上核验该账单。检查范围见 [docs/proofs.md](docs/proofs.md)。它不能证明上游供应商报告的 token 数是真实的。

**状态。** 工作区版本是 0.1.0，还没有 GitHub Release。运营者上手仍有缺口，跟踪在 [#725](https://github.com/AllureCurtain/oxsum/issues/725)。`verify-oxsum-bill` 尚未发布到 npm（2026-11-12 的 `npm view verify-oxsum-bill` 返回 404）；请从 [`web/verify-sdk`](web/verify-sdk) 构建。[oxsum.785777.xyz](https://oxsum.785777.xyz) 不是受支持的试用环境：2026-11-12 它重定向到 `/app` 并返回 HTML，`GET /api/v1/version` 没有返回版本文档。这次探测没有确认提交、余额、凭据或供应商连通性。本仓库里的界面图片是 2026-10-24 的仪表盘，不是当前界面。拉取请求上变绿的 CI 只是快速层，不是完整的候选发布检查（[候选门槛](docs/self-hosted-release.md)）。

本页只覆盖首屏、状态、本地启动和证明边界。功能列表、架构、演示脚本和其余文档以 [README.md](README.md) 为准。索引在 [docs/README.md](docs/README.md)。

## 进程先跑起来

需要不低于工作区 MSRV 1.98 的 Rust stable，以及带 Compose 的 Docker。`.env.example` 里的 `DATABASE_URL` 已经指向 `compose.yaml` 创建的数据库。

```bash
git clone https://github.com/AllureCurtain/oxsum.git
cd oxsum
cp .env.example .env
docker compose up -d
cargo run -p oxsum-server
```

第一次 `cargo run` 会编译整个工作区。然后查看 [健康检查](http://127.0.0.1:3000/healthz) 和 [就绪检查](http://127.0.0.1:3000/readyz)。停在这里时，数据库里还没有 Business 所有者。

## 成为所有者

在 `.env` 里取消 `OXSUM_ADMIN_TOKEN` 的注释，设为 `openssl rand -hex 24` 的输出（至少 16 个字符），然后重启。签发一次性邀请：

```bash
curl -s -X POST http://127.0.0.1:3000/api/v1/admin/business/setup-tokens \
  -H "authorization: Bearer $OXSUM_ADMIN_TOKEN" \
  -H "content-type: application/json" \
  -d '{"idempotencyKey":"setup-1","ownerEmail":"owner@example.com"}'
```

回答里的 `token` 只出现一次。打开 [http://127.0.0.1:3000/app/setup](http://127.0.0.1:3000/app/setup) 接受，再到 [http://127.0.0.1:3000/app](http://127.0.0.1:3000/app) 登录并选择 Business。部署令牌不要放进客户浏览器。镜像部署用 [`deploy/`](deploy/docker-compose.core.yml) 和[安装手册](docs/self-hosted-install.md)，端点与上面相同。

`OXSUM_SECRET_KEY`（`openssl rand -base64 32`）在数据库保存供应商或支付凭据之前必须设置。`OXSUM_HEAD_SIGNING_KEY` 可选；不设置时签名日志路由返回 503。客户注册在所有者显式开放之前保持仅邀请。`OXSUM_SIGNUP` 不能覆盖已保存的策略。

## 一份证明能确立什么

留存的内容哈希让 `verify_bundle` 检查条目字节是否仍哈希到该值，以及包含证明是否到达所给的日志头。对验证器认识的结算描述，另一次金额检查会按条目内的用量和费率重算记录下的金额。签名的价格绑定和销售条款绑定、一致性证明、用量报告或证据包、见证方联合签名，各自回答不同的问题。安装了 WASM 时，`/verify` 在浏览器里跑检查；否则使用服务端表单。

同一次检查不能证明供应商确实返回了那么多 token，不能证明和证明包一起送来的公钥是独立可信的运营者身份，也不能证明见证方在组织上独立。完整对照表在 [docs/proofs.md](docs/proofs.md)。客户步骤在 [docs/verify-your-bill.md](docs/verify-your-bill.md)。

## 接着读

- [英文 README](README.md)：功能、架构、隔离演示、FAQ
- [文档索引](docs/README.md)
- [证明范围](docs/proofs.md)
- [安装](docs/self-hosted-install.md)
- [许可证](README.md#license)：MIT OR Apache-2.0

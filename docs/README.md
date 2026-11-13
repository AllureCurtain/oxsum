# Documentation index

Use this page to pick a document. Longer pages disagree with themselves in places; the rule below says which sentence wins.

| You want | Read |
| --- | --- |
| What oxsum is, in one sitting | [Product overview](product-page.md) |
| Who can do what, and how a call is billed | [Product behavior](product.md). The sections near the top, and any section marked shipped, are the current behavior. The historical scope note in that file is not a second spec. |
| Install an image | [Self-hosted install](self-hosted-install.md), then [upgrade and restore](self-hosted-upgrade.md) |
| Run a checkout | The repository [README](../README.md) ("process is up", then "become the owner") and [development](development.md) |
| Check a bill | [Verify your bill](verify-your-bill.md), then the boundary in [proofs](proofs.md) |
| See an interface | [API](api.md) and `crates/server/openapi.yaml`. Refusals are [unsupported](unsupported.md) |
| Embed gateway or metering | [Integrations](integrations.md) and `examples/` |
| Payments | [Rails](rails.md). A local fixture is not a live-money certification |
| Why a technical choice was made | [Decisions](decisions.md). The **Status** line on an entry wins. Superseded entries stay as history and are not current instructions |
| Operator launch status | [Operator launch](roadmap/operator-launch/README.md). Screenshot and JSON captures are indexed in [EVIDENCE.md](roadmap/EVIDENCE.md) once that note is on the branch you have; the markdown conclusions are the part to read |
| Run an operator procedure | `runbooks/` — rehearsal and ops runbooks, e.g. [Solana mainnet rehearsal](runbooks/solana-mainnet-rehearsal.md) |
| Release gates | [Self-hosted release](self-hosted-release.md) |
| Day-to-day UI | [User guide](user-guide.md) and [DESIGN.md](../DESIGN.md) |
| Security or support | [SECURITY.md](../SECURITY.md), [SUPPORT.md](../SUPPORT.md) |
| What is unfinished | [TODO.md](../TODO.md), one row per open issue |

`product-page.md` is the short overview. `product.md` is the long behavior specification. `decisions.md` is the decision record, not a second product spec.

## Directory map

| Path | What it is |
| --- | --- |
| `crates/core` | Domain: wallet, sale terms, ledger use, identity |
| `crates/server` | HTTP, the gateway, and the `oxsum` binary |
| `crates/verify` | Pure proof checks, shared with the browser. wasm32-clean |
| `crates/web` | Package `oxsum-web`. Server-rendered `/verify` and `/bill/{token}` only. Not the dashboard |
| `crates/integrations` | GitHub OAuth, Stripe, mail, and the other outbound clients |
| `crates/doubleentry` | Vendored ledger engine. Change it only under the rules in `AGENTS.md` |
| `web/` | React dashboard, built into the binary and served at `/app` |
| `demo/` | One isolated local script, `demo/demo.py`. Not the dashboard pictures |
| `examples/` | Maintained walkthroughs (scoped management, consumption, webhooks, the Solana rail, the offline verifier) |
| `deploy/` | Compose profiles for an image install |
| `docs/roadmap/` | Launch and release notes. Conclusions stay in markdown. Captures are not required reading |
| `docs/runbooks/` | Operator runbooks: rehearsals and procedures that move real money or real state |
| `TODO.md` | Open-issue index. `docs/history/TODO-log.md` is a frozen narrative, not a place to append |

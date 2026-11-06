# Protocol support and verification scope

This is the T07 starter matrix. T13/T26 must complete the operation, parameter, SDK and cost evidence before certification. Local loopback fixtures establish software behavior; they are not real-provider certification.

| Surface | Channel protocol | Request class | Current permissions and limits | Local evidence |
| --- | --- | --- | --- | --- |
| `/v1/chat/completions` | `openai` | Whole response and SSE chat | Customer/key models, group/business channels, alias targets, experiment and fallback choices; signed mandate | `server/tests/gateway.rs`, `fallbacks.rs`, `experiments.rs`, `groups.rs` |
| `/v1/responses` | `openai` | Responses-compatible whole/SSE | Same shared gateway admission; supported parsing/adapters only | `server/tests/gateway.rs` |
| `/v1/messages` | `anthropic` | Messages whole/SSE | Same shared admission; Anthropic version header and usage adapter | `server/tests/gateway.rs` |
| `/v1beta/models/{model}:generateContent`, `:streamGenerateContent` | `gemini` | Gemini whole/SSE | Same shared admission; model path encoded as data | `server/tests/gateway.rs` |
| `/v1/embeddings` | `openai` | Nonstreamed text or supported token input | Same admission; embeddings price mode | `server/tests/gateway.rs` |
| `/v1/rerank` | `openai` | Nonstreamed query/documents | Same admission; rerank price mode | `server/tests/gateway.rs` |
| `/v1/tasks` | `tasks` | Generic async submit/status/cancel adapter | Direct model only; customer tier/key/group; exact originating key for reads/cancel; original endpoint snapshot. Aliases and signed mandates explicitly refused pending T13 binding | `server/tests/tasks.rs` |
| `/api/v1/metering/*` | Service event contract | Authorized external event usage | Scoped integration credential/customer grant; consumption keys refused | `core/tests/metering.rs`, `server/tests/integration_credentials.rs` |

`/v1/models` lists only customer/key-permitted models, including direct task models. This listing alone does not claim image/audio/video provider compatibility. Customer pricing, estimates and direct signed-price checkpoints also filter permissions; exported historical proofs keep their own scope.

Aliases require the public name and resolved upstream target in every non-null model allowlist. Null is unrestricted. Writes require a nonempty model list; a legacy empty list fails closed. Customer sessions see their tier's catalog; a consumption key narrows it further with its own models and group channels. Unknown/revoked/expired/foreign keys never become unrestricted. Experimental variants and fallbacks do not grant access to denied original names, targets or channels.

T07.b replaces the retired normalized-text cache key with a versioned HMAC of the complete request and admitted context; pre-upgrade rows are discarded and `excludeSystem` no longer narrows the question. T08–T13 still own customer sale terms, durable operations, zero-cost admission, funding and final per-attempt cost semantics. Real paid-provider/SDK certification is pending separate credentials and authorization. No unsupported modality is inferred from the existence of the generic task API.

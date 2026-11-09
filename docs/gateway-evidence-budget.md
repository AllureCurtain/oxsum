# Bounded gateway settlement evidence

Implementation contract for #875, building on #873. This workflow is not yet
integrated or validated. Existing historical entries and v3–v14 verification
remain unchanged.

A gateway request must be able to retain its complete billing and provenance
record before model execution starts. The gateway accepts at most 16,384 bytes
of serialized UTF-8 settlement JSON. This is deliberately stricter than the
ledger's 16,384 Unicode-scalar limit: JSON escaping, multibyte characters and
all composed metadata count toward admission.

New channel/key/mandate policy-name collections contain at most eight distinct
valid names. The effective union also has at most eight policies, with up to
sixteen pre/post verdicts. An existing oversized union refuses before moderation
or model execution, quota draw, hold or operation creation. Operators narrow the
configuration explicitly; the server does not shorten a list or skip its tail.

The admission check uses the same v15 encoder as settlement. It tests the full
upper envelope for every possible resolved route: exact requested, served and
upstream model names, channel and experiment variant, the longest serialized
price condition, supported bill lines and numeric widths, both guardrail phases,
and mandate, subscription, cache, sale and error evidence. All field collections
are finite. Maximal schema tests must change with any new encoder field or
priced dimension. A policy-count check alone is not a capacity proof.

## Full reasons and commitments

A complete reason whose JSON string is no more than 256 UTF-8 bytes remains
inline as `reason`. A larger reason omits `reason` and carries:

```json
{"reasonCommitment":{"sha256":"<64 lowercase hex>","utf8Bytes":1234}}
```

The digest covers the exact original UTF-8 bytes, including whitespace and line
breaks. The byte length is an integer. The server does not emit a shortened string
as though it were the full reason. Both a guardrail verdict and a top-level
failure reason use this representation; policy, verdict, streamed status and
all monetary/provenance fields remain present.

The full text for a committed reason is retained in the usage evidence beside
its exact verdict index, policy and verdict, or failure-reason identity. It commits
in the same transaction as the settlement. Moderation no longer silently removes
text after 150 characters. Its separate response reader has a size limit; an
oversized or unreadable response is an evaluation error under the existing
pre-call refusal/post-call fail-open policy, never an invented policy verdict.
A pre-call refusal takes no hold and uses bounded refusal evidence. Its existing
best-effort marker failure cannot authorize execution or imply success.

The v15 verifier validates reason/commitment exclusivity and shape. A separately
supplied full reason can be compared with the committed digest and UTF-8 byte
length. The bill or period bundle's original entry bytes commit this digest even
when the bundle does not carry the full reason. Without that text, the verifier
cannot claim it has compared the reason. Neither result proves that the policy's
judgment was correct or that the provider was honest.

Tests must cover escaped and multibyte text, maximum and oversized unions,
whole-response/stream/cache/fallback paths, pre/post flag and block, exact replay,
atomic rollback and native/browser verification of genuine and altered evidence.
No source test or local fixture substitutes for release-candidate acceptance.

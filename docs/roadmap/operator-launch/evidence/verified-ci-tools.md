# Verified CI tool bootstrap

Issue [#822](https://github.com/AllureCurtain/oxsum/issues/822), November 8,
2026. This repairs setup failures before tests run; it does not replace a
required check or constitute release-candidate acceptance.

## Observed failures

PR #817's run `42464323384`, attempts 1 and 2, failed at `Install mold` before
compilation. `curl -sL` piped a non-gzip response into privileged tar. Local
reproduction with HTTP failure reporting showed persistent HTTP 504 at the
browser release URL. PR #821's run `42469026254` then failed in the Mozilla
sccache action while downloading 0.19.0 from the browser release URL, also
with repeated HTTP 504. The official GitHub release asset API returned both
archives successfully.

## Pinned official bytes

| Tool | Official release asset ID | Bytes | SHA-256 |
| --- | --- | --- | --- |
| mold 2.40.0 x86_64 Linux | `rui314/mold`, `258128381` | 5,083,010 | `143e9d9624a2a7ffa25565a8bd3818479029c83b4f5913ed11dd2acaace3fab3` |
| sccache 0.19.0 x86_64 Linux musl | `mozilla/sccache`, `671951070` | 9,879,308 | `aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd` |

Metadata was read from the official
[mold release API](https://api.github.com/repos/rui314/mold/releases/tags/v2.40.0)
and [sccache release API](https://api.github.com/repos/mozilla/sccache/releases/tags/v0.19.0).
Downloads use `Accept: application/octet-stream` on the exact asset API URL.
The mold release's older asset metadata has `digest: null`; two independent
downloads matched the retained hash and metadata size. This is a reviewed
pin of official retrieved bytes, not a signed publisher digest. Sccache's
download matches the SHA-256 that its GitHub asset metadata reports.

## Behavior and verification

The shared installer makes at most four curl attempts, with HTTPS-only
redirects, ten-second connection and sixty-second per-attempt timeouts, plus
a 250-second whole-process cap. HTTP failure, truncation, unexpected size and
hash mismatch never reach extraction. A private staging directory retains
all files until the archive is validated; safe regular files precede bounded
relative symlinks. A complete directory is renamed atomically into the runner
tool cache, followed by its completion marker. No existing install is partly
overwritten and no privileged extraction is used.

Mold's bin directory is added through `GITHUB_PATH`. Sccache is staged as
`sccache/0.19.0/x64` with sibling `x64.complete`. Mozilla action v0.0.11 is
retained with input `version: v0.19.0`. Its reviewed `setup.ts` calls
`find('sccache', version)`, and Actions tool-cache normalizes the input with
`semver.clean`, matching this layout. The action retains `SCCACHE_PATH`,
`ACTIONS_CACHE_SERVICE_V2`, runtime cache context and post-run statistics.

Local checks passed:

- Ten controlled tests using a loopback HTTPS server, real curl, and a test
  certificate: transient 504 recovery, four-attempt persistent failure,
  HTML/truncation, equal-size corruption, process timeout cleanup, unchanged
  existing installs, verified replay, unsafe archive rejection and workflow
  pin/guard coverage.
- Both actual official assets passed the complete download, size/hash,
  extraction and atomic installation path in disposable local directories.
  The macOS ARM host did not execute the Linux binaries; required Linux CI
  performs their version checks and the Mozilla cache lookup.
- Actionlint 1.7.11 accepted both changed workflows. Pinned formatting and
  whitespace checks passed. The seven existing candidate-CI checks passed.
- Independent source review found no blocker in verification ordering,
  tool-cache layout, cache-environment retention or workflow conditions.

All required fast checks and full candidate jobs keep their original meaning.
The candidate workflow checks out its selected source before invoking this
helper, so new runs require a revision containing the helper; it does not run
unrelated newer code against an old candidate source.

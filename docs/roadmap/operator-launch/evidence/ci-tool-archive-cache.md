# Verified CI archive reuse (#828)

PR #824's pinned API download worked on Linux, but subsequent #821, #823 and
#825 required jobs encountered repeated official HTTP 502/503/504 responses.
One failed-job retry on #821 also failed before compilation. These are retained
acquisition failures, not passing Rust checks.

The follow-up keeps both original archive hashes and versions. An exact Actions
cache key includes installer contents, tool, OS and architecture. Each restore
is copied into private staging and reverified before extraction. Each tool is
saved independently immediately after verified installation. Cold downloads
may fall back to the corresponding official release URL on bounded transport
failure only; oversized or mismatched payloads fail closed.

Local validation covers real loopback HTTPS retry/fallback, the eight-request
ceiling across both URLs, integrity/oversize refusal, offline restored archive
installation, private archive permissions, corrupt/symlink cache refusal and
the existing archive traversal/atomic installation regressions. No public
artifact or candidate is promoted by these checks. Actual workflow success is
recorded separately from local acquisition fixtures.

# Roadmap evidence captures

The markdown under `docs/roadmap/` is the conclusion: what was checked, what passed, and what was still open. Screenshot, JSON, log, SSE, checksum, and one-off fixture scripts that sat beside those notes were removed from the working tree on 2026-11-12 so a clone does not carry them.

They were not rewritten out of history. The tree that still contains every removed file is commit `8ac212c5900cde5cef58967145c43e060a966dfb` (the `main` tip immediately before this removal).

Retrieve one file:

```bash
git show 8ac212c5900cde5cef58967145c43e060a966dfb:docs/roadmap/operator-launch/evidence/sale-browser/runtime.json
```

List what that commit had under a directory:

```bash
git ls-tree -r --name-only 8ac212c5900cde5cef58967145c43e060a966dfb docs/roadmap/operator-launch/evidence/sale-browser
```

A markdown note that links `runtime.json` or `changed-price-mobile.png` is describing that historical capture. The link does not resolve in the current tree. The numbers and verdicts written in the markdown itself are the part this repository still publishes.

This removal does not change a conclusion, promote a capture to a release candidate, or publish a GitHub Release.

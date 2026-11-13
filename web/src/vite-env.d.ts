/// <reference types="vite/client" />

// The dashboard's first `import.meta.env` read (issue #1147,
// `VITE_SOLANA_RPC_URL` in `lib/solanaTx.ts`) needs Vite's own ambient types —
// `ImportMeta.env` is declared here rather than hand-rolled so every future
// `VITE_*` read type-checks the same way.

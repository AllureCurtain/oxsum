import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The SPA is served by the binary under /app/ (crates/server/src/spa.rs); the
// base keeps every emitted asset URL under that prefix.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: "/app/",
  build: {
    outDir: "dist",
  },
  server: {
    // Development talks to the locally running server
    // (`cargo run -p oxsum-server`) — same-origin in production, proxied here.
    proxy: {
      "/api": "http://localhost:3000",
      "/dashboard": "http://localhost:3000",
      "/verify": "http://localhost:3000",
      "/ws": { target: "http://localhost:3000", ws: true },
    },
  },
});

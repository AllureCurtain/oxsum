import { fileURLToPath, URL } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The SPA is served by the binary under /app/ (crates/server/src/spa.rs); the
// base keeps every emitted asset URL under that prefix.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  base: "/app/",
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  build: {
    outDir: "dist",
    rollupOptions: {
      output: {
        // Vendor chunks keep the app code thin and let the browser cache the
        // slow-moving libraries separately.
        manualChunks: {
          "react-vendor": ["react", "react-dom", "react-router-dom", "@tanstack/react-query"],
          motion: ["motion"],
          "ui-vendor": [
            "lucide-react",
            "@radix-ui/react-tabs",
            "@radix-ui/react-select",
            "@radix-ui/react-switch",
            "@radix-ui/react-progress",
            "@radix-ui/react-slot",
            "@radix-ui/react-dialog",
            "@radix-ui/react-alert-dialog",
            "@radix-ui/react-dropdown-menu",
            "@radix-ui/react-popover",
            "@radix-ui/react-tooltip",
            "@radix-ui/react-hover-card",
            "@radix-ui/react-scroll-area",
            "@radix-ui/react-separator",
            "@radix-ui/react-label",
            "@radix-ui/react-checkbox",
            "@radix-ui/react-radio-group",
            "@radix-ui/react-avatar",
            "@radix-ui/react-toggle",
            "sonner",
          ],
        },
      },
    },
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

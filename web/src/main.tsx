import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import { App } from "./App";
import { Toaster } from "./components/ui";
import { TooltipProvider } from "./components/ui";
import "./i18n";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Money pages re-fetch on focus: a stale balance is worse than a fetch.
      refetchOnWindowFocus: true,
      retry: 1,
    },
  },
});

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <TooltipProvider>
        <BrowserRouter basename="/app">
          <App />
        </BrowserRouter>
        <Toaster position="top-right" />
      </TooltipProvider>
    </QueryClientProvider>
  </StrictMode>,
);

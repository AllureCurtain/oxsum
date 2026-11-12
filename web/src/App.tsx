import { lazy, type ComponentType, type ReactNode } from "react";
import { ApplicationLink } from "./pages/auth/ApplicationLink";
import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/AppShell";
import { PageBoundary } from "./components/PageBoundary";
import { withChunkRecovery } from "./lib/chunk-reload";
import {
  Device,
  ForgotPassword,
  Login,
  RedeemInvitation,
  Register,
  ResetPassword,
  Setup,
  VerifyEmail,
} from "./pages/auth";
import { Overview } from "./pages/Overview";

// Code splitting (issue #1092): every page inside the shell except the
// overview and device pages lazy-loads, so the ~1 MB single chunk becomes an
// entry plus per-route-group chunks — a reader of one page no longer
// downloads the whole admin and business consoles. The auth pages stay
// eager: they are the first paint a logged-out visitor needs.
//
// One lazy() per route keeps the chunk per page; the import path *is* the
// chunk the bundler emits. `L` wraps each lazy page in `PageBoundary`: a
// Suspense boundary for the chunk load, and an error boundary that shows a
// reload prompt in the page slot when the chunk cannot load.

/**
 * `lazy()` over a page import that recovers when the page's chunk is gone
 * (`lib/chunk-reload.ts`).
 */
function lazyPage<T extends ComponentType>(
  load: () => Promise<{ default: T }>,
) {
  return lazy(withChunkRecovery(load));
}

const Admin = lazyPage(() =>
  import("./pages/Admin").then((m) => ({ default: m.Admin })),
);
const Business = lazyPage(() =>
  import("./pages/Business").then((m) => ({ default: m.Business })),
);
const Billing = lazyPage(() =>
  import("./pages/Billing").then((m) => ({ default: m.Billing })),
);
const BillingStatement = lazyPage(() =>
  import("./pages/Billing").then((m) => ({ default: m.BillingStatement })),
);
const Bills = lazyPage(() =>
  import("./pages/Bills").then((m) => ({ default: m.Bills })),
);
const Chat = lazyPage(() =>
  import("./pages/Chat").then((m) => ({ default: m.Chat })),
);
const Evidence = lazyPage(() =>
  import("./pages/Evidence").then((m) => ({ default: m.Evidence })),
);
const GettingStarted = lazyPage(() =>
  import("./pages/GettingStarted").then((m) => ({ default: m.GettingStarted })),
);
const Keys = lazyPage(() =>
  import("./pages/Keys").then((m) => ({ default: m.Keys })),
);
const Log = lazyPage(() =>
  import("./pages/Log").then((m) => ({ default: m.Log })),
);
const Members = lazyPage(() =>
  import("./pages/Members").then((m) => ({ default: m.Members })),
);
const Orders = lazyPage(() =>
  import("./pages/Orders").then((m) => ({ default: m.Orders })),
);
const OrderNew = lazyPage(() =>
  import("./pages/Orders").then((m) => ({ default: m.OrderNew })),
);
const OrderDetail = lazyPage(() =>
  import("./pages/Orders").then((m) => ({ default: m.OrderDetail })),
);
const Plans = lazyPage(() =>
  import("./pages/Plans").then((m) => ({ default: m.Plans })),
);
const Pricing = lazyPage(() =>
  import("./pages/Pricing").then((m) => ({ default: m.Pricing })),
);
const Requests = lazyPage(() =>
  import("./pages/Requests").then((m) => ({ default: m.Requests })),
);
const Reports = lazyPage(() =>
  import("./pages/Reports").then((m) => ({ default: m.Reports })),
);
const Settings = lazyPage(() =>
  import("./pages/Settings").then((m) => ({ default: m.Settings })),
);
const Statements = lazyPage(() =>
  import("./pages/Statements").then((m) => ({ default: m.Statements })),
);
const StatementDetail = lazyPage(() =>
  import("./pages/Statements").then((m) => ({ default: m.StatementDetail })),
);
const Topup = lazyPage(() =>
  import("./pages/Topup").then((m) => ({ default: m.Topup })),
);
const Usage = lazyPage(() =>
  import("./pages/Usage").then((m) => ({ default: m.Usage })),
);
const Webhooks = lazyPage(() =>
  import("./pages/Webhooks").then((m) => ({ default: m.Webhooks })),
);

/// The boundaries around a lazy page: while its chunk loads, the shell frame
/// stays and the page slot shows the shared empty state.
function L(element: ReactNode) {
  return <PageBoundary>{element}</PageBoundary>;
}

export function App() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route path="/register" element={<Register />} />
      <Route path="/application-link" element={<ApplicationLink />} />
      <Route path="/invite" element={<RedeemInvitation />} />
      <Route path="/setup" element={<Setup />} />
      <Route path="/forgot-password" element={<ForgotPassword />} />
      <Route path="/reset-password" element={<ResetPassword />} />
      <Route path="/verify-email" element={<VerifyEmail />} />
      <Route element={<AppShell />}>
        <Route index element={<Overview />} />
        <Route path="getting-started" element={L(<GettingStarted />)} />
        <Route path="topup" element={L(<Topup />)} />
        <Route path="pricing" element={L(<Pricing />)} />
        <Route path="plans" element={L(<Plans />)} />
        <Route path="billing" element={L(<Billing />)} />
        <Route path="billing/:id" element={L(<BillingStatement />)} />
        <Route path="settings" element={L(<Settings />)} />
        <Route path="bills" element={L(<Bills />)} />
        <Route path="evidence" element={L(<Evidence />)} />
        <Route path="requests" element={L(<Requests />)} />
        <Route path="usage" element={L(<Usage />)} />
        <Route path="keys" element={L(<Keys />)} />
        <Route path="members" element={L(<Members />)} />
        <Route path="webhooks" element={L(<Webhooks />)} />
        <Route path="statements" element={L(<Statements />)} />
        <Route path="statements/:id" element={L(<StatementDetail />)} />
        <Route path="orders" element={L(<Orders />)} />
        <Route path="orders/new" element={L(<OrderNew />)} />
        <Route path="orders/:id" element={L(<OrderDetail />)} />
        <Route path="reports" element={L(<Reports />)} />
        <Route path="log" element={L(<Log />)} />
        <Route path="chat" element={L(<Chat />)} />
        <Route path="device" element={<Device />} />
        <Route path="admin" element={L(<Admin />)} />
        <Route path="business" element={L(<Business />)} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

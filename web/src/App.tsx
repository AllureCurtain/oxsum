import { Suspense, lazy, type ReactNode } from "react";
import { ApplicationLink } from "./pages/auth/ApplicationLink";
import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/AppShell";
import { PageState } from "./components/ui";
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
// landing page lazy-loads, so the ~1 MB single chunk becomes an entry plus
// per-route-group chunks — a reader of one page no longer downloads the
// whole admin and business consoles. The auth pages stay eager: they are
// the first paint a logged-out visitor needs.
//
// One lazy() per route keeps the chunk per page; the import path *is* the
// chunk the bundler emits. `L` gives each lazy page its own Suspense
// boundary so one slow chunk never blanks the shell's other routes.
const Admin = lazy(() =>
  import("./pages/Admin").then((m) => ({ default: m.Admin })),
);
const Business = lazy(() =>
  import("./pages/Business").then((m) => ({ default: m.Business })),
);
const Billing = lazy(() =>
  import("./pages/Billing").then((m) => ({ default: m.Billing })),
);
const BillingStatement = lazy(() =>
  import("./pages/Billing").then((m) => ({ default: m.BillingStatement })),
);
const Bills = lazy(() =>
  import("./pages/Bills").then((m) => ({ default: m.Bills })),
);
const Chat = lazy(() =>
  import("./pages/Chat").then((m) => ({ default: m.Chat })),
);
const Evidence = lazy(() =>
  import("./pages/Evidence").then((m) => ({ default: m.Evidence })),
);
const GettingStarted = lazy(() =>
  import("./pages/GettingStarted").then((m) => ({ default: m.GettingStarted })),
);
const Keys = lazy(() =>
  import("./pages/Keys").then((m) => ({ default: m.Keys })),
);
const Log = lazy(() => import("./pages/Log").then((m) => ({ default: m.Log })));
const Members = lazy(() =>
  import("./pages/Members").then((m) => ({ default: m.Members })),
);
const Orders = lazy(() =>
  import("./pages/Orders").then((m) => ({ default: m.Orders })),
);
const OrderNew = lazy(() =>
  import("./pages/Orders").then((m) => ({ default: m.OrderNew })),
);
const OrderDetail = lazy(() =>
  import("./pages/Orders").then((m) => ({ default: m.OrderDetail })),
);
const Plans = lazy(() =>
  import("./pages/Plans").then((m) => ({ default: m.Plans })),
);
const Pricing = lazy(() =>
  import("./pages/Pricing").then((m) => ({ default: m.Pricing })),
);
const Requests = lazy(() =>
  import("./pages/Requests").then((m) => ({ default: m.Requests })),
);
const Reports = lazy(() =>
  import("./pages/Reports").then((m) => ({ default: m.Reports })),
);
const Settings = lazy(() =>
  import("./pages/Settings").then((m) => ({ default: m.Settings })),
);
const Statements = lazy(() =>
  import("./pages/Statements").then((m) => ({ default: m.Statements })),
);
const StatementDetail = lazy(() =>
  import("./pages/Statements").then((m) => ({ default: m.StatementDetail })),
);
const Topup = lazy(() =>
  import("./pages/Topup").then((m) => ({ default: m.Topup })),
);
const Usage = lazy(() =>
  import("./pages/Usage").then((m) => ({ default: m.Usage })),
);
const Webhooks = lazy(() =>
  import("./pages/Webhooks").then((m) => ({ default: m.Webhooks })),
);

/// One Suspense boundary around a lazy page: while its chunk loads, the
/// shell frame stays and the page slot shows the shared empty state.
function L(element: ReactNode) {
  return <Suspense fallback={<PageState />}>{element}</Suspense>;
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

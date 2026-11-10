import { ApplicationLink } from "./pages/auth/ApplicationLink";
import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/AppShell";
import { Admin } from "./pages/Admin";
import { Business } from "./pages/Business";
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
import { Billing, BillingStatement } from "./pages/Billing";
import { Evidence } from "./pages/Evidence";
import { Bills } from "./pages/Bills";
import { Chat } from "./pages/Chat";
import { GettingStarted } from "./pages/GettingStarted";
import { Keys } from "./pages/Keys";
import { Log } from "./pages/Log";
import { Members } from "./pages/Members";
import { OrderDetail, OrderNew, Orders } from "./pages/Orders";
import { Overview } from "./pages/Overview";
import { Plans } from "./pages/Plans";
import { Pricing } from "./pages/Pricing";
import { Requests } from "./pages/Requests";
import { Reports } from "./pages/Reports";
import { Settings } from "./pages/Settings";
import { StatementDetail, Statements } from "./pages/Statements";
import { Topup } from "./pages/Topup";
import { Usage } from "./pages/Usage";
import { Webhooks } from "./pages/Webhooks";

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
        <Route path="getting-started" element={<GettingStarted />} />
        <Route path="topup" element={<Topup />} />
        <Route path="pricing" element={<Pricing />} />
        <Route path="plans" element={<Plans />} />
        <Route path="billing" element={<Billing />} />
        <Route path="billing/:id" element={<BillingStatement />} />
        <Route path="settings" element={<Settings />} />
        <Route path="bills" element={<Bills />} />
        <Route path="evidence" element={<Evidence />} />
        <Route path="requests" element={<Requests />} />
        <Route path="usage" element={<Usage />} />
        <Route path="keys" element={<Keys />} />
        <Route path="members" element={<Members />} />
        <Route path="webhooks" element={<Webhooks />} />
        <Route path="statements" element={<Statements />} />
        <Route path="statements/:id" element={<StatementDetail />} />
        <Route path="orders" element={<Orders />} />
        <Route path="orders/new" element={<OrderNew />} />
        <Route path="orders/:id" element={<OrderDetail />} />
        <Route path="reports" element={<Reports />} />
        <Route path="log" element={<Log />} />
        <Route path="chat" element={<Chat />} />
        <Route path="device" element={<Device />} />
        <Route path="admin" element={<Admin />} />
        <Route path="business" element={<Business />} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

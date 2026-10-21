import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/AppShell";
import { Admin } from "./pages/Admin";
import {
  Device,
  ForgotPassword,
  Login,
  RedeemInvitation,
  Register,
  ResetPassword,
  VerifyEmail,
} from "./pages/Auth";
import { Bills } from "./pages/Bills";
import { Chat } from "./pages/Chat";
import { Keys } from "./pages/Keys";
import { Log } from "./pages/Log";
import { Members } from "./pages/Members";
import { Overview } from "./pages/Overview";
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
      <Route path="/invite" element={<RedeemInvitation />} />
      <Route path="/forgot-password" element={<ForgotPassword />} />
      <Route path="/reset-password" element={<ResetPassword />} />
      <Route path="/verify-email" element={<VerifyEmail />} />
      <Route element={<AppShell />}>
        <Route index element={<Overview />} />
        <Route path="topup" element={<Topup />} />
        <Route path="pricing" element={<Pricing />} />
        <Route path="settings" element={<Settings />} />
        <Route path="bills" element={<Bills />} />
        <Route path="requests" element={<Requests />} />
        <Route path="usage" element={<Usage />} />
        <Route path="keys" element={<Keys />} />
        <Route path="members" element={<Members />} />
        <Route path="webhooks" element={<Webhooks />} />
        <Route path="statements" element={<Statements />} />
        <Route path="statements/:id" element={<StatementDetail />} />
        <Route path="reports" element={<Reports />} />
        <Route path="log" element={<Log />} />
        <Route path="chat" element={<Chat />} />
        <Route path="device" element={<Device />} />
        <Route path="admin" element={<Admin />} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

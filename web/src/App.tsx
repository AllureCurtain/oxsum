import { Navigate, Route, Routes } from "react-router-dom";
import { AppShell } from "./components/AppShell";
import { Bills } from "./pages/Bills";
import { Keys } from "./pages/Keys";
import { Login } from "./pages/Login";
import { Overview } from "./pages/Overview";
import { Placeholder } from "./pages/Placeholder";
import { Usage } from "./pages/Usage";

export function App() {
  return (
    <Routes>
      <Route path="/login" element={<Login />} />
      <Route element={<AppShell />}>
        <Route index element={<Overview />} />
        <Route path="bills" element={<Bills />} />
        <Route path="requests" element={<Placeholder title="Requests" topcoat="/dashboard/requests" />} />
        <Route path="usage" element={<Usage />} />
        <Route path="keys" element={<Keys />} />
        <Route path="members" element={<Placeholder title="Members" topcoat="/dashboard/members" />} />
        <Route path="webhooks" element={<Placeholder title="Webhooks" topcoat="/dashboard/webhooks" />} />
        <Route path="statements" element={<Placeholder title="Statements" topcoat="/dashboard/statements" />} />
      </Route>
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}

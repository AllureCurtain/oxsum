import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button, Card, Field, TextInput } from "../../components/ui";
import { ApiError } from "../../api/client";
import { adminApi } from "./api";

export function Users({ token }: { token: string }) {
  const { t } = useTranslation();
  const [userId, setUserId] = useState("");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<string | null>(null);
  return (
    <Card title={t("admin.users.resetTitle")}>
      <form
        className="flex flex-col gap-3"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setMsg(null);
          try {
            const res = await adminApi<{ userId: string; sessionsRevoked: number }>(
              token,
              `/users/${userId}/password-reset`,
              { method: "POST", body: { newPassword: password } },
            );
            setMsg(t("admin.users.done", { count: res.sessionsRevoked }));
          } catch (err) {
            setMsg(err instanceof ApiError ? err.message : "failed");
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className="text-sm text-muted">
          {t("admin.users.note")}
        </p>
        <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
          <Field label={t("admin.users.userId")}>
            <TextInput
              value={userId}
              onChange={(e) => setUserId(e.target.value)}
              placeholder="uuid"
              className="mono"
              required
            />
          </Field>
          <Field label={t("admin.users.newPassword")} hint={t("admin.users.newPasswordHint")}>
            <TextInput
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              minLength={12}
              required
            />
          </Field>
        </div>
        <div className="flex items-center gap-3">
          <Button type="submit" isLoading={busy} loadingText={t("admin.users.resetting")}>
            {t("admin.users.resetPassword")}
          </Button>
          {msg ? <span className="text-sm text-muted">{msg}</span> : null}
        </div>
      </form>
    </Card>
  );
}

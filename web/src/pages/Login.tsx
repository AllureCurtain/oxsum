import { useState, type FormEvent } from "react";
import { useNavigate } from "react-router-dom";
import { useLogin } from "../api/hooks";
import { ApiError } from "../api/client";

export function Login() {
  const login = useLogin();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");

  function submit(e: FormEvent) {
    e.preventDefault();
    login.mutate({ email, password }, { onSuccess: () => navigate("/", { replace: true }) });
  }

  const error = login.error instanceof ApiError ? login.error.message : null;
  return (
    <main className="mx-auto mt-24 w-full max-w-sm">
      <div className="rounded-md border border-line bg-surface p-6">
        <h1 className="text-2xl font-bold">oxsum</h1>
        <p className="mb-4 mt-1 text-sm text-muted">Sign in to your organization.</p>
        <form onSubmit={submit} className="flex flex-col gap-3">
          <label className="flex flex-col gap-1 text-sm">
            Email
            <input
              type="email"
              required
              autoComplete="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              className="rounded border border-line bg-surface px-2 py-2"
            />
          </label>
          <label className="flex flex-col gap-1 text-sm">
            Password
            <input
              type="password"
              required
              autoComplete="current-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              className="rounded border border-line bg-surface px-2 py-2"
            />
          </label>
          {error ? <p className="text-sm text-danger">{error}</p> : null}
          <button
            type="submit"
            disabled={login.isPending}
            className="rounded bg-primary px-3 py-2 font-semibold text-surface hover:bg-primary-hover disabled:opacity-60"
          >
            {login.isPending ? "Signing in…" : "Sign in"}
          </button>
        </form>
      </div>
    </main>
  );
}

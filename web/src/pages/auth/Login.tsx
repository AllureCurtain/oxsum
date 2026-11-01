import { useState, type FormEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useAuthMethods, useLogin, useTotpLogin } from "../../api/hooks";
import { Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

export function Login() {
  const login = useLogin();
  const totp = useTotpLogin();
  const methods = useAuthMethods();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  // When the first leg answers `requiresTotp`, the page swaps the password
  // form for a code field and carries the challenge forward — no session was
  // created, so nothing to invalidate on that arm.
  const [challengeToken, setChallengeToken] = useState<string | null>(null);
  const [code, setCode] = useState("");

  function submit(e: FormEvent) {
    e.preventDefault();
    login.mutate(
      { email, password },
      {
        onSuccess: (data) => {
          if ("requiresTotp" in data && data.requiresTotp) {
            setChallengeToken(data.challengeToken);
          } else {
            navigate("/", { replace: true });
          }
        },
      },
    );
  }

  function submitTotp(e: FormEvent) {
    e.preventDefault();
    if (!challengeToken) return;
    totp.mutate(
      { challengeToken, code: code.trim() },
      { onSuccess: () => navigate("/", { replace: true }) },
    );
  }

  if (challengeToken) {
    return (
      <AuthCard title="Two-factor authentication">
        <form onSubmit={submitTotp} className="flex flex-col gap-3">
          <p className="text-sm text-muted">
            Enter the six-digit code from your authenticator, or a single-use
            backup code.
          </p>
          <Field label="Authentication code">
            <TextInput
              inputMode="numeric"
              autoComplete="one-time-code"
              required
              autoFocus
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="123456"
            />
          </Field>
          <ErrorText error={totp.error} />
          <Button type="submit" disabled={totp.isPending}>
            {totp.isPending ? "Verifying…" : "Verify"}
          </Button>
          <button
            type="button"
            className="text-sm text-primary"
            onClick={() => {
              setChallengeToken(null);
              setCode("");
              totp.reset();
            }}
          >
            Back to sign in
          </button>
        </form>
      </AuthCard>
    );
  }

  return (
    <AuthCard title="Sign in">
      <form onSubmit={submit} className="flex flex-col gap-3">
        <Field label="Email">
          <TextInput
            type="email"
            required
            autoComplete="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
        </Field>
        <Field label="Password">
          <TextInput
            type="password"
            required
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
        <ErrorText error={login.error} />
        <Button type="submit" disabled={login.isPending}>
          {login.isPending ? "Signing in…" : "Sign in"}
        </Button>
      </form>
      {methods.data?.oauthGithub ? (
        <a
          href="/api/v1/auth/oauth/github"
          className="mt-3 block rounded-lg border border-line px-3 py-2 text-center text-sm hover:bg-canvas"
        >
          Sign in with GitHub
        </a>
      ) : null}
      {methods.data?.oidcProviders.map((name) => (
        <a
          key={name}
          href={`/api/v1/auth/oauth/${encodeURIComponent(name)}`}
          className="mt-3 block rounded-lg border border-line px-3 py-2 text-center text-sm hover:bg-canvas"
        >
          Sign in with {name.replace(/^oidc:/, "")}
        </a>
      ))}
      <p className="mt-4 flex justify-between text-sm">
        <Link to="/forgot-password" className="text-primary">
          Forgot password
        </Link>
        <Link to="/register" className="text-primary">
          Register
        </Link>
      </p>
    </AuthCard>
  );
}

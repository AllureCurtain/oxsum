import { useState, type FormEvent, type ReactNode } from "react";
import { Link, useNavigate, useSearchParams } from "react-router-dom";
import { useMutation } from "@tanstack/react-query";
import { ApiError, api } from "../api/client";
import { useAuthMethods, useLogin } from "../api/hooks";
import { FractalDotGrid } from "../components/cult/bg-animated-fractal-dot-grid";
import { Banner, Button, Field, TextInput } from "../components/ui";

/** The centered card every session page shares, over the animated dot grid. */
function AuthCard(props: { title: string; children: ReactNode }) {
  return (
    <main className="relative flex min-h-screen items-start justify-center overflow-hidden">
      <div className="absolute inset-0 opacity-60">
        <FractalDotGrid
          dotColor="rgba(37, 99, 235, 1)"
          glowColor="rgba(37, 99, 235, 1)"
          dotOpacity={0.25}
          waveIntensity={18}
          enableMouseGlow={false}
          initialPerformance="low"
        />
      </div>
      <div className="relative z-10 mt-24 w-full max-w-sm px-4">
        <div className="rounded-md border border-line bg-surface p-6">
          <h1 className="text-2xl font-bold">{props.title}</h1>
          <div className="mt-4">{props.children}</div>
        </div>
      </div>
    </main>
  );
}

function ErrorText({ error }: { error: unknown }) {
  return error instanceof ApiError ? (
    <p className="text-sm text-danger">{error.message}</p>
  ) : null;
}

export function Login() {
  const login = useLogin();
  const methods = useAuthMethods();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");

  function submit(e: FormEvent) {
    e.preventDefault();
    login.mutate({ email, password }, { onSuccess: () => navigate("/", { replace: true }) });
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
          className="mt-3 block rounded-md border border-line px-3 py-2 text-center text-sm hover:bg-canvas"
        >
          Sign in with GitHub
        </a>
      ) : null}
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

export function Register() {
  const methods = useAuthMethods();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [orgName, setOrgName] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const register = useMutation({
    mutationFn: (body: { email: string; password: string; organizationName?: string; turnstileToken?: string | null }) =>
      api<{ apiKey: { secret: string }; verificationSent: boolean }>("/auth/register", {
        method: "POST",
        body,
      }),
  });

  function submit(e: FormEvent) {
    e.preventDefault();
    register.mutate(
      {
        email,
        password,
        organizationName: orgName || undefined,
        // The Turnstile widget renders only when a site key is configured; this
        // scaffold leaves the token absent and lets the server decide.
        turnstileToken: null,
      },
      { onSuccess: (data) => setSecret(data.apiKey.secret) },
    );
  }

  if (secret) {
    return (
      <AuthCard title="Registered">
        <Banner tone="success">
          Your first API key — stored only as a hash, shown once:
          <code className="mono ml-2 break-all">{secret}</code>
        </Banner>
        <Button className="mt-4 w-full" onClick={() => navigate("/login")}>
          To sign in
        </Button>
      </AuthCard>
    );
  }

  return (
    <AuthCard title="Register">
      <form onSubmit={submit} className="flex flex-col gap-3">
        <Field label="Email">
          <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label="Password" hint="at least 12 characters">
          <TextInput
            type="password"
            required
            minLength={12}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
        <Field label="Organization name" hint="defaults to your email's local part">
          <TextInput value={orgName} maxLength={80} onChange={(e) => setOrgName(e.target.value)} />
        </Field>
        <ErrorText error={register.error} />
        <Button type="submit" disabled={register.isPending}>
          {register.isPending ? "Registering…" : "Register"}
        </Button>
      </form>
      {methods.data?.oauthGithub ? (
        <a
          href="/api/v1/auth/oauth/github"
          className="mt-3 block rounded-md border border-line px-3 py-2 text-center text-sm hover:bg-canvas"
        >
          Register with GitHub
        </a>
      ) : null}
      <p className="mt-4 text-sm">
        <Link to="/login" className="text-primary">
          Already have an account
        </Link>
      </p>
    </AuthCard>
  );
}

export function ForgotPassword() {
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState(false);
  const forgot = useMutation({
    mutationFn: (body: { email: string }) =>
      api("/auth/password/forgot", { method: "POST", body }),
  });

  return (
    <AuthCard title="Reset password">
      {sent ? (
        <Banner tone="success">
          If the account exists, a reset mail is on its way.
        </Banner>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            forgot.mutate({ email }, { onSuccess: () => setSent(true) });
          }}
        >
          <Field label="Email">
            <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
          </Field>
          <ErrorText error={forgot.error} />
          <Button type="submit" disabled={forgot.isPending}>
            Send reset mail
          </Button>
        </form>
      )}
      <p className="mt-4 text-sm">
        <Link to="/login" className="text-primary">
          Back to sign in
        </Link>
      </p>
    </AuthCard>
  );
}

export function ResetPassword() {
  const [params] = useSearchParams();
  const [password, setPassword] = useState("");
  const [done, setDone] = useState(false);
  const reset = useMutation({
    mutationFn: (body: { token: string; password: string }) =>
      api("/auth/password/reset", { method: "POST", body }),
  });
  const token = params.get("token") ?? "";

  return (
    <AuthCard title="Choose a new password">
      {done ? (
        <>
          <Banner tone="success">The password was changed. All sessions were revoked.</Banner>
          <Button className="mt-4 w-full" onClick={() => undefined}>
            <Link to="/login" className="block w-full text-center text-white">
              Sign in
            </Link>
          </Button>
        </>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            reset.mutate({ token, password }, { onSuccess: () => setDone(true) });
          }}
        >
          <Field label="New password" hint="at least 12 characters">
            <TextInput
              type="password"
              required
              minLength={12}
              autoComplete="new-password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </Field>
          <ErrorText error={reset.error} />
          <Button type="submit" disabled={reset.isPending || !token}>
            Reset password
          </Button>
        </form>
      )}
    </AuthCard>
  );
}

export function VerifyEmail() {
  const [params] = useSearchParams();
  const verify = useMutation({
    mutationFn: (token: string) => api("/auth/verify", { method: "POST", body: { token } }),
  });
  const [asked, setAsked] = useState(false);
  const token = params.get("token") ?? "";

  return (
    <AuthCard title="Verify email">
      {verify.isSuccess ? (
        <Banner tone="success">Your email is verified.</Banner>
      ) : (
        <>
          {token ? (
            <Button onClick={() => verify.mutate(token)} disabled={verify.isPending}>
              Verify this address
            </Button>
          ) : (
            <>
              <p className="text-sm text-muted">
                No token in the link? Request a fresh verification mail.
              </p>
              <Button
                className="mt-3"
                disabled={asked}
                onClick={() => {
                  api("/auth/verify/request", { method: "POST" }).then(() => setAsked(true));
                }}
              >
                {asked ? "Mail sent" : "Send verification mail"}
              </Button>
            </>
          )}
          <ErrorText error={verify.error} />
        </>
      )}
    </AuthCard>
  );
}

export function Device() {
  const [params] = useSearchParams();
  const [code, setCode] = useState(params.get("code") ?? "");
  const [done, setDone] = useState<"approved" | "denied" | null>(null);
  const authorize = useMutation({
    mutationFn: (body: { userCode: string; approve: boolean }) =>
      api("/device/authorize", { method: "POST", body }),
  });

  return (
    <AuthCard title="Authorize a device">
      {done ? (
        <Banner tone="success">
          {done === "approved" ? "The device may poll for its key." : "The request was denied."}
        </Banner>
      ) : (
        <form
          className="flex flex-col gap-3"
          onSubmit={(e) => {
            e.preventDefault();
            authorize.mutate({ userCode: code, approve: true }, { onSuccess: () => setDone("approved") });
          }}
        >
          <Field label="User code" hint="the code the tool shows">
            <TextInput
              required
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="XXXX-XXXX"
            />
          </Field>
          <ErrorText error={authorize.error} />
          <div className="flex gap-2">
            <Button type="submit" disabled={authorize.isPending}>
              Approve
            </Button>
            <Button
              tone="danger"
              type="button"
              disabled={authorize.isPending}
              onClick={() =>
                authorize.mutate({ userCode: code, approve: false }, { onSuccess: () => setDone("denied") })
              }
            >
              Deny
            </Button>
          </div>
        </form>
      )}
    </AuthCard>
  );
}

export function RedeemInvitation() {
  const [params] = useSearchParams();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const redeem = useMutation({
    mutationFn: (body: { token: string; email: string; password: string }) =>
      api("/invitations/redeem", { method: "POST", body }),
  });
  const token = params.get("token") ?? "";

  return (
    <AuthCard title="Join the organization">
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          redeem.mutate({ token, email, password }, { onSuccess: () => navigate("/login") });
        }}
      >
        <Field label="Email">
          <TextInput type="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label="Password" hint="at least 12 characters">
          <TextInput
            type="password"
            required
            minLength={12}
            autoComplete="new-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </Field>
        <ErrorText error={redeem.error} />
        <Button type="submit" disabled={redeem.isPending || !token}>
          Register into the organization
        </Button>
      </form>
    </AuthCard>
  );
}

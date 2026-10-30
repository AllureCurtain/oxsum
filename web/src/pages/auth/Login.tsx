import { useState, type FormEvent } from "react";
import { Link, useNavigate } from "react-router-dom";
import { useAuthMethods, useLogin } from "../../api/hooks";
import { Button, Field, TextInput } from "../../components/ui";
import { AuthCard, ErrorText } from "./shared";

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
          className="mt-3 block rounded-lg border border-line px-3 py-2 text-center text-sm hover:bg-canvas"
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

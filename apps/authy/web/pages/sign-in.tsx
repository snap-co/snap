import { useRef, useState, type FormEvent } from "react";
import { Link } from "@tanstack/react-router";
import { AuthyClient } from "../client";
import { AuthShell, AuthHeading, AuthActions, AuthButton, AuthField } from "../auth-ui";

export function SignInPage({ client }: { client: AuthyClient }) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [mode, setMode] = useState<"signin" | "signup">("signin");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submitting = useRef(false);
  const [visible, setVisible] = useState(false);
  const passkey = async () => {
    if (submitting.current) return;
    if (mode === "signup" && !/^[^\s@]+@[^\s@]+$/.test(email)) { setError("Enter your email before creating a passkey account."); return; }
    submitting.current = true;
    setBusy(true);
    setError(null);
    try { await client.passkey(mode === "signup", email); setPassword(""); }
    catch (e) { setError(e instanceof Error ? e.message : "Passkey sign-in failed. Try again or use your password."); }
    finally { submitting.current = false; setBusy(false); }
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (submitting.current) return;
    setError(null);
    const bytes = new TextEncoder().encode(password).length;
    if (mode === "signup" && (bytes < 8 || bytes > 1024)) {
      setError(bytes < 8 ? "Choose a longer password: use at least 8 bytes." : "Your password is too long. Use no more than 1,024 bytes.");
      return;
    }
    submitting.current = true;
    setBusy(true);
    try {
      await (mode === "signup" ? client.signup(email, password) : client.login(email, password));
      setPassword("");
    } catch (e) {
      setError(e instanceof Error ? e.message : "We couldn't sign you in. Check your connection and try again.");
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };

  return (
    <AuthShell brand={<Link className="brand" to="/" aria-label="Authy home">Snap <span className="brand-divider">/</span> Authy</Link>}>
      <AuthHeading title={mode === "signup" ? "Create your account" : "Sign in"}>One account for your Snap apps.</AuthHeading>
      <form onSubmit={submit} className="signin-form" aria-busy={busy}>
        <AuthField label="Email" id="email">
        <input
          id="email"
          name="email"
          type="email"
          autoComplete="username"
          autoCapitalize="none"
          spellCheck={false}
          readOnly={busy}
          required
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        </AuthField>
        <AuthField label="Password" id="password">
        <div className="password-control">
        <input
          id="password"
          name="password"
          type={visible ? "text" : "password"}
          autoComplete={mode === "signup" ? "new-password" : "current-password"}
          aria-describedby={mode === "signup" ? "password-hint" : undefined}
          readOnly={busy}
          required
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <AuthButton type="button" secondary aria-controls="password" aria-label={visible ? "Hide password" : "Show password"} onClick={() => setVisible(!visible)}>{visible ? "Hide" : "Show"}</AuthButton>
        </div>
        {mode === "signup" && <small id="password-hint">Use a unique password, 8–1,024 bytes. A typical letter or number uses one byte.</small>}
        </AuthField>
        <AuthActions>
          <AuthButton type="submit" disabled={busy}>
            {busy ? (mode === "signup" ? "Creating account…" : "Signing in…") : (mode === "signup" ? "Create account" : "Sign in")}
          </AuthButton>
          <AuthButton
            type="button"
            secondary
            className="mode-switch"
            disabled={busy}
            onClick={() => {
              setMode(mode === "signup" ? "signin" : "signup");
              setError(null);
              setPassword("");
              setVisible(false);
            }}
          >
            {mode === "signup" ? "Have an account? Sign in" : "New here? Create account"}
          </AuthButton>
        </AuthActions>
        {client.passkeysSupported() && <AuthActions><AuthButton type="button" secondary disabled={busy} onClick={() => void passkey()}>{mode === "signup" ? "Create account with a passkey" : "Sign in with a passkey"}</AuthButton></AuthActions>}
      </form>
      <p className="submission-status" role="status">{busy ? (mode === "signup" ? "Creating your account…" : "Checking your sign-in details…") : ""}</p>
      {error && <p role="alert">{error}</p>}
    </AuthShell>
  );
}

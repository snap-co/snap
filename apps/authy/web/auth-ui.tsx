import { type ReactNode, type ButtonHTMLAttributes } from "react";

/** Shared by the interactive account UI and build-time rendered protocol pages.
 * Keep presentation here; neither view owns authentication or redirect policy. */
export function AuthShell({ children, status }: { children: ReactNode; status?: ReactNode }) {
  return <main className="auth-shell"><header className="auth-header"><a className="brand" href="/" aria-label="Authy home"><span className="brand-mark" aria-hidden="true">S</span>Snap <span className="brand-divider">/</span> Authy</a>{status}</header>{children}<footer className="auth-footer">Your account for Snap apps.</footer></main>;
}

export function AuthHeading({ title, children }: { title: string; children?: ReactNode }) {
  return <div className="auth-heading"><h1>{title}</h1>{children && <p className="muted">{children}</p>}</div>;
}

export function AuthActions({ children }: { children: ReactNode }) {
  return <div className="actions">{children}</div>;
}

export function AuthButton({ secondary = false, className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { secondary?: boolean }) {
  return <button {...props} className={`${secondary ? "secondary" : "primary"} ${className}`.trim()} />;
}

export function Permission({ title, description }: { title: string; description: string }) {
  return <li className="permission"><span className="permission-check" aria-hidden="true">✓</span><div><strong>{title}</strong><small>{description}</small></div></li>;
}

// Slots are filled once by the native host with escaped request data. These are
// ordinary HTML forms, so consent and logout work with scripts disabled.
export function ConsentPage() {
  return <AuthShell><AuthHeading title="Authorize application"><strong>{"{{client}}"}</strong> is requesting access to your Authy account.</AuthHeading>
    <div className="application-identity"><span className="muted">Continue to</span><strong>{"{{client}}"}</strong><span className="application-origin">{"{{origin}}"}</span></div>
    <h2>This application will be able to</h2><ul className="permissions">{"{{permissions}}"}</ul>
    <p className="muted consent-note">Your password is never shared with the application.</p>
    <form method="post" action="/oauth/authorize"><input type="hidden" name="request" value="{{request}}"/><AuthActions><AuthButton name="decision" value="allow">Allow</AuthButton><AuthButton secondary name="decision" value="deny">Deny</AuthButton></AuthActions></form>
  </AuthShell>;
}

export function LogoutPage() {
  return <AuthShell><AuthHeading title="Sign out of Authy">End your current sign-in session on this browser?</AuthHeading><p className="muted">You can sign in again whenever you need to.</p><form method="post" action="/oauth/logout"><input type="hidden" name="request" value="{{request}}"/><AuthActions><AuthButton>Confirm sign out</AuthButton><a className="button secondary" href="/">Stay signed in</a></AuthActions></form></AuthShell>;
}

export function AuthErrorPage() {
  return <AuthShell><AuthHeading title="We couldn't complete this request">{"{{message}}"}</AuthHeading><p className="muted">Return to the application and try again, or check your Authy session.</p><AuthActions><a className="button primary" href="/">Return to Authy</a></AuthActions></AuthShell>;
}

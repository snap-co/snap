import { type ReactNode, type ButtonHTMLAttributes } from "react";

/** Shared by the interactive account UI and build-time rendered protocol pages.
 * Keep presentation here; neither view owns authentication or redirect policy. */
export function AuthShell({ children, status, brand }: { children: ReactNode; status?: ReactNode; brand?: ReactNode }) {
  return <main className="auth-shell"><header className="auth-header">{brand ?? <a className="brand" href="/" aria-label="Authy home">Snap <span className="brand-divider">/</span> Authy</a>}{status}</header>{children}<footer className="auth-footer">Your account for Snap apps.</footer></main>;
}

export function AuthHeading({ title, children }: { title: string; children?: ReactNode }) {
  return <div className="auth-heading"><h1>{title}</h1>{children && <p className="muted">{children}</p>}</div>;
}

export function AuthActions({ children }: { children: ReactNode }) {
  return <div className="actions">{children}</div>;
}

export function AuthField({ label, id, children }: { label: string; id: string; children: ReactNode }) {
  return <div className="auth-field"><label htmlFor={id}>{label}</label>{children}</div>;
}

export function AuthButton({ secondary = false, className = "", ...props }: ButtonHTMLAttributes<HTMLButtonElement> & { secondary?: boolean }) {
  return <button {...props} className={`${secondary ? "secondary" : "primary"} ${className}`.trim()} />;
}

export function Permission({ title, description }: { title: string; description: string }) {
  return <li className="permission"><svg className="permission-check" aria-hidden="true" viewBox="0 0 20 20" fill="none"><path d="m4 10 4 4 8-8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round"/></svg><div><strong>{title}</strong><small>{description}</small></div></li>;
}

// Slots are filled once by the native host with escaped request data. These are
// ordinary HTML forms, so consent and logout work with scripts disabled.
export function ConsentPage() {
  return <AuthShell><AuthHeading title="Authorize application"><strong>{"{{client}}"}</strong> is requesting access to your Authy account.</AuthHeading>
    <dl className="application-identity"><dt>Signed in as</dt><dd>{"{{email}}"}</dd><dt>Continue to</dt><dd><strong>{"{{client}}"}</strong><span className="application-origin">{"{{origin}}"}</span></dd></dl>
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

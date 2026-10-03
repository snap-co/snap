import { startRegistration, startAuthentication, browserSupportsWebAuthn, WebAuthnAbortService, type PublicKeyCredentialCreationOptionsJSON, type PublicKeyCredentialRequestOptionsJSON } from "@simplewebauthn/browser";

/** Only the platform ceremony lives here. Rust's Identity SDK owns the
 * operations, and the browser keeps session cookies out of JavaScript. */
export interface PasskeyBinding {
  identity_passkey_register(label: string, binding: string): Promise<string>;
  identity_passkey_registered(proof: string): Promise<string>;
  identity_passkey_authenticate(locator: string | undefined, name: string | undefined, binding: string): Promise<string>;
  identity_passkey_authenticated(proof: string): Promise<string>;
}
type Challenge<T> = { attempt: string; options: { publicKey: T }; expires: number };
function binding(): string {
  return Array.from(crypto.getRandomValues(new Uint8Array(32)), byte => byte.toString(16).padStart(2, "0")).join("");
}
export function bindPasskeys<M extends PasskeyBinding, A>(load: () => Promise<M>, account: (module: M) => Promise<A>) {
  return {
    supported: () => browserSupportsWebAuthn() && !location.hostname.includes(":") && !/^[\d.]+$/.test(location.hostname) && (location.protocol === "https:" || location.hostname === "localhost"),
    cancel: () => WebAuthnAbortService.cancelCeremony(),
    async register(label: string): Promise<A> {
      const module = await load();
      const secret = binding();
      const challenge = JSON.parse(await module.identity_passkey_register(label, secret)) as Challenge<PublicKeyCredentialCreationOptionsJSON>;
      const response = await startRegistration({ optionsJSON: challenge.options.publicKey });
      await module.identity_passkey_registered(JSON.stringify({ attempt: challenge.attempt, binding: secret, response }));
      return account(module);
    },
    async authenticate(name?: string, locator?: string): Promise<A> {
      const module = await load();
      const secret = binding();
      const challenge = JSON.parse(await module.identity_passkey_authenticate(locator, name?.trim() || undefined, secret)) as Challenge<PublicKeyCredentialRequestOptionsJSON>;
      const response = await startAuthentication({ optionsJSON: challenge.options.publicKey });
      await module.identity_passkey_authenticated(JSON.stringify({ attempt: challenge.attempt, binding: secret, response }));
      return account(module);
    },
  };
}

import { Transport, TransportError } from "../transport/browser";

export interface Credentials { email: string; password: string }

/** Identity's pre-connection SDK. HTTP is a Transport carrier; applications
 * never create credential endpoints or handle bearer/cookie material. */
export class Identity<Account> {
  constructor(private readonly transport: Transport) {}

  fetch(): Promise<Account | null> {
    return this.transport.request<Account | null>("identity.fetch", "GET");
  }

  acquire(credentials: Credentials): Promise<{ account: Account }> {
    return this.credentials("identity.acquire", credentials);
  }

  enroll(credentials: Credentials): Promise<{ account: Account }> {
    return this.credentials("identity.enroll", credentials);
  }

  private async credentials(operation: string, credentials: Credentials): Promise<{ account: Account }> {
    try {
      return await this.transport.request(operation, "POST", credentials);
    } catch (error) {
      const messages: Record<number, string> = {
        400: "Check your email address and password requirements, then try again.",
        401: "The email or password is incorrect. Check both and try again.",
        403: "This request couldn't be verified. Reload the page and try again.",
        409: "That email is already registered; sign in instead.",
        429: "Too many attempts. Wait a moment before trying again.",
      };
      if (error instanceof TransportError) throw new Error(messages[error.status] ?? "We couldn't complete your sign-in. Please try again shortly.");
      throw new Error("We couldn't reach the server. Check your connection and try again.");
    }
  }
}

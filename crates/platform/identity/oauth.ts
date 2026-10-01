/** The relying-party host owns OAuth refresh and CSRF. Its session projection
 * adapts to the same fetch/null contract as password Identity, without borrowing
 * browser refresh authority for explicit agent tokens. */
export class OAuthIdentity<A extends { identified: boolean }> {
  constructor(private readonly origin = "", private readonly request: typeof fetch = globalThis.fetch.bind(globalThis)) {}
  async fetch(): Promise<A | null> {
    const response = await this.request(`${this.origin}/api/session`, { credentials: "same-origin" });
    if (!response.ok) throw new Error(`Session check failed (${response.status})`);
    const session = await response.json() as A;
    if (typeof session.identified !== "boolean") throw new Error("Invalid Identity session");
    return session.identified ? session : null;
  }
}

// Browser carrier for Transport: HTTP before connection, WebSocket after
// Identity resolves. Cookie handling belongs to the browser, not application JS.
export class TransportError extends Error {
  constructor(readonly status: number, readonly outcome: unknown) {
    super(`Transport request failed (${status})`);
  }
}

export class Transport {
  private readonly clientId = Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join("");
  private nextId = 0;

  async request<T>(operation: string, method: "GET" | "POST", input?: unknown): Promise<T> {
    const id = ++this.nextId;
    const response = await fetch(`/${operation.replaceAll(".", "/")}`, {
      method,
      credentials: "same-origin",
      headers: { accept: "application/json", "x-snap-operation-id": String(id), ...(method === "POST" ? { "content-type": "application/json" } : {}) },
      body: method === "POST" ? JSON.stringify(input) : undefined,
    });
    const frame = await response.json() as { Completed?: { id: number; outcome: { Ok?: T; Err?: unknown } } };
    const completed = frame.Completed;
    if (!completed || completed.id !== id || !completed.outcome) throw new Error("Invalid Transport completion");
    if (!response.ok || "Err" in completed.outcome) throw new TransportError(response.status, completed.outcome.Err);
    if (!("Ok" in completed.outcome)) throw new Error("Missing Transport result");
    return completed.outcome.Ok as T;
  }

  /** Only call after Identity acquisition/fetch succeeds. The platform owns the
   * stable runtime client ID and injects it into the SDK's Connect command. */
  connect(command: (clientId: string) => string): WebSocket {
    const socket = new WebSocket(`${location.origin.replace(/^http/, "ws")}/transport`);
    socket.addEventListener("open", () => {
      try { socket.send(command(this.clientId)); }
      catch { socket.close(); }
    }, { once: true });
    return socket;
  }
}

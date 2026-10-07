// Browser WebSocket glue. Rust owns HTTP requests and protocol behavior;
// browser-managed cookies authenticate the socket upgrade.
export type Connected = (operation: string, input: string) => Promise<string>;

export class Transport {
  private readonly clientId = Array.from(crypto.getRandomValues(new Uint8Array(16)), byte => byte.toString(16).padStart(2, "0")).join("");

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

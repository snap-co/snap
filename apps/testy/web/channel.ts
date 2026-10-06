// The carrier handles IO only. Rust owns command construction, correlation,
// bootstrap and calculator contracts. Frames remain text to preserve i64 values.
//
// The channel is a stream, not a request/response exchange: `send` writes one
// command and returns, `receive` yields the next frame whenever it arrives.
// Transport carries a single event per frame, so an operation spanning
// acceptance, progress and completion crosses the socket as three frames and a
// slow operation publishes as it runs instead of holding its whole answer.
export class WebChannel {
  private socket: WebSocket;
  // Frames that arrived with no one waiting. Dropping them would lose a push
  // that lands between calls, so they wait here for the next `receive`.
  private queue: string[] = [];
  private waiter?: {
    resolve: (value: string) => void;
    reject: (error: Error) => void;
  };
  // Request spans acceptance, credential handoff and completion. Correlate the
  // whole exchange; a frame count would expose its second authentication frame.
  private authentication = new Set<number>();
  private connecting = false;
  readonly ready: Promise<void>;
  constructor(
    private observe: (frame: string) => void,
    private lost: () => void,
  ) {
    this.socket = new WebSocket(
      `${location.origin.replace("http", "ws")}/transport`,
    );
    this.ready = new Promise((resolve, reject) => {
      this.socket.onopen = () => resolve();
      this.socket.onerror = () => reject(new Error("Cannot connect to Testy"));
    });
    this.socket.onclose = () => {
      this.waiter?.reject(
        new Error("Connection lost; outstanding outcome is unknown"),
      );
      this.waiter = undefined;
      this.queue.length = 0;
      this.authentication.clear();
      this.connecting = false;
      this.lost();
    };
    this.socket.onmessage = ({ data }) => {
      let decoded;
      try {
        decoded = JSON.parse(data);
      } catch (error) {
        this.fail(error instanceof Error ? error : new Error(String(error)));
        return;
      }
      const event = decoded?.Event;
      const id = event?.Accepted?.id ?? event?.Progress?.id ?? event?.Bearer?.id ?? event?.Completed?.id;
      const handshake = this.connecting && typeof decoded === "object" && decoded !== null && ("Attached" in decoded || "Failed" in decoded);
      // Bearer handoffs are always private, including unexpected IDs.
      const redact = Boolean(event?.Bearer) || this.authentication.has(id) || handshake;
      this.observe(redact ? "← [authentication response redacted]" : data);
      if (event?.Completed) this.authentication.delete(event.Completed.id);
      if (handshake) this.connecting = false;
      const waiter = this.waiter;
      if (!waiter) {
        this.queue.push(data);
        return;
      }
      this.waiter = undefined;
      waiter.resolve(data);
    };
  }

  // Writes one command. Returns once it is on the socket, not once the operation
  // is accepted or finished.
  async send(command: string): Promise<void> {
    await this.ready;
    if (this.socket.readyState !== WebSocket.OPEN)
      throw new Error("Channel unavailable");
    const decoded = JSON.parse(command);
    if (decoded.Connect) this.connecting = true;
    if (decoded.Request) this.authentication.add(decoded.Request.invocation.id);
    this.observe(
      decoded.Connect || decoded.Request ? "→ [authentication request redacted]" : `→ ${command}`,
    );
    this.socket.send(command);
  }

  // The next frame, whenever it arrives. Rejects if the socket closes, because
  // an unknown outcome must not be reported as a completed call.
  receive(): Promise<string> {
    const buffered = this.queue.shift();
    if (buffered !== undefined) return Promise.resolve(buffered);
    return new Promise((resolve, reject) => {
      if (this.socket.readyState !== WebSocket.OPEN) {
        reject(new Error("Channel unavailable"));
        return;
      }
      this.waiter = { resolve, reject };
    });
  }

  private fail(error: Error) {
    this.waiter?.reject(error);
    this.waiter = undefined;
    this.queue.length = 0;
    this.socket.close();
  }

  dispose() {
    this.socket.close();
  }
}

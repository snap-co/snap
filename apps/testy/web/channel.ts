// The carrier handles IO only. Rust owns command construction, correlation,
// bootstrap and calculator contracts. Frames remain text to preserve i64 values.
export class WebChannel {
  private socket: WebSocket;
  private pending?: {
    resolve: (value: string) => void;
    reject: (error: Error) => void;
    events: string[];
  };
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
      this.pending?.reject(
        new Error("Connection lost; request outcome may be unknown"),
      );
      this.pending = undefined;
      this.lost();
    };
    this.socket.onmessage = ({ data }) => {
      this.observe(data);
      const pending = this.pending;
      if (!pending) {
        this.socket.close();
        return;
      }
      try {
        const frame = JSON.parse(data);
        if (frame.Events) {
          // Server sends one observation per frame. Preserve its original numeric text.
          if (
            !Array.isArray(frame.Events) ||
            frame.Events.length !== 1 ||
            pending.events.length >= 2
          )
            throw new Error("Invalid event frame");
          const match = /^\{"Events":\[(.*)\]\}$/.exec(data);
          if (!match) throw new Error("Invalid event encoding");
          pending.events.push(match[1]);
          if (!frame.Events[0].Completed) return;
          pending.resolve(`{"Events":[${pending.events.join(",")}]}`);
        } else {
          if (pending.events.length)
            throw new Error("Interrupted event sequence");
          pending.resolve(data);
        }
        this.pending = undefined;
      } catch (error) {
        pending.reject(
          error instanceof Error ? error : new Error(String(error)),
        );
        this.pending = undefined;
        this.socket.close();
      }
    };
  }
  async exchange(command: string): Promise<string> {
    await this.ready;
    if (this.pending || this.socket.readyState !== WebSocket.OPEN)
      throw new Error("Channel unavailable");
    this.observe(`→ ${command}`);
    return new Promise((resolve, reject) => {
      this.pending = { resolve, reject, events: [] };
      this.socket.send(command);
    });
  }
  dispose() {
    this.socket.close();
  }
}

import {
  development_control,
  development_observation,
} from "../.snap/web/bindings/testy_wasm.js";

export type Host = {
  manual: boolean;
  breakpoint: boolean;
  program: string;
  snapshot: boolean;
  active: {
    ticket: string;
    operation: string;
    accepted: boolean;
    waiting: string | null;
  } | null;
  queued: unknown[];
  states: string;
  trace: string;
};
export type DebuggerStatus = "Connecting" | "Live" | "Reconnecting";

/** A host observation connection, never an application peer. Reconnection gets a
 * fresh full report. Pending commands fail on loss and are never auto-replayed. */
export class DevelopmentChannel {
  private socket?: WebSocket;
  private stopped = false;
  private ready = false;
  private sequence = 0;
  private delay = 250;
  private retry?: ReturnType<typeof setTimeout>;
  private pending = new Map<
    string,
    {
      resolve: (result: unknown) => void;
      reject: (error: Error) => void;
      timer: ReturnType<typeof setTimeout>;
    }
  >();

  constructor(
    private observe: (host: Host) => void,
    private status: (status: DebuggerStatus) => void,
  ) {
    this.connect();
  }
  private connect() {
    const socket = new WebSocket(
      `${location.origin.replace("http", "ws")}/__dev/ws`,
    );
    this.socket = socket;
    socket.onmessage = ({ data }) => {
      if (this.stopped || socket !== this.socket) return;
      try {
        const frame = JSON.parse(development_observation(data));
        if (frame.type === "state") {
          this.ready = true;
          this.delay = 250;
          this.observe(frame.state);
          this.status("Live");
        } else if (frame.type === "result") {
          const pending = this.pending.get(frame.id);
          if (!pending) throw new Error("Unknown debugger command response");
          this.pending.delete(frame.id);
          clearTimeout(pending.timer);
          if (frame.error !== undefined) pending.reject(new Error(frame.error));
          else pending.resolve(frame.result);
        } else throw new Error("Invalid debugger frame");
      } catch {
        socket.close();
      }
    };
    socket.onclose = () => {
      if (this.stopped || socket !== this.socket) return;
      this.ready = false;
      this.rejectPending();
      this.status("Reconnecting");
      this.retry = setTimeout(() => this.connect(), this.delay);
      this.delay = Math.min(this.delay * 2, 4000);
    };
  }
  command(action: object, input?: string): Promise<unknown> {
    if (!this.ready || this.socket?.readyState !== WebSocket.OPEN) {
      return Promise.reject(new Error("Debugger is disconnected"));
    }
    // Embed Rust-validated JSON text without parsing its numeric values in JS.
    const control = development_control(JSON.stringify(action), input);
    const id = String(++this.sequence);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.ready = false;
        this.rejectPending();
        this.socket?.close();
      }, 10_000);
      this.pending.set(id, { resolve, reject, timer });
      this.socket!.send(`{"id":${JSON.stringify(id)},"control":${control}}`);
    });
  }
  private rejectPending() {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(
        new Error("Debugger connection lost; command outcome may be unknown"),
      );
    }
    this.pending.clear();
  }
  dispose() {
    this.stopped = true;
    this.ready = false;
    clearTimeout(this.retry);
    this.rejectPending();
    this.socket?.close();
  }
}

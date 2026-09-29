import { Invocations } from "../document/client";
import { Transport } from "../transport/browser";

export interface Binding {
  connect(id: string): string;
  invoke(operation: string, input: string): string;
  receive(frame: string): string;
  free(): void;
}
export interface Publication {
  send: string[];
  /** Rust's manifest/recovery state, never inferred from document count. */
  ready: boolean;
  error?: string | null;
}
export type SessionState<A> = Readonly<{
  phase: "loading" | "anonymous" | "ready" | "error";
  account: A | null;
  connection: "disconnected" | "connecting" | "connected";
  error: string | null;
  epoch: number;
}>;
export interface SessionRuntime<A> {
  getSnapshot(): SessionState<A>;
  subscribe(listener: () => void): () => void;
  resolve(): Promise<SessionState<A>>;
  refresh(): Promise<SessionState<A>>;
  close(): void;
}

/** One identity-owned binding and logical connection. Physical reconnects retain
 * the binding and published pages. Actual identity changes clear both immediately.
 * Async fetches, binding creation and socket callbacks are fenced by epoch. */
export class BrowserRuntime<A, B extends Binding, P extends Publication> implements SessionRuntime<A> {
  private state: SessionState<A> = { phase: "loading", account: null, connection: "disconnected", error: null, epoch: 0 };
  private listeners = new Set<() => void>();
  private binding?: B;
  private socket?: WebSocket;
  private pending?: Promise<SessionState<A>>;
  private timer?: ReturnType<typeof setTimeout>;
  private deadline?: ReturnType<typeof setTimeout>;
  private closed = false;
  private backoff = 250;
  private transport: Pick<Transport, "connect">;
  private calls = new Invocations((operation, input) => {
    if (!this.binding || this.state.connection !== "connected") throw new Error("Disconnected");
    return this.binding.invoke(operation, JSON.stringify(input));
  }, frame => this.send(frame));

  constructor(private readonly options: {
    identity: { fetch(): Promise<A | null> };
    key(account: A): string;
    create(account: A): Promise<B>;
    decode(raw: string): P;
    publish(value: P | null): void;
    transport?: Pick<Transport, "connect">;
    timeout?: number;
  }) { this.transport = options.transport ?? new Transport(); }

  getSnapshot = () => this.state;
  subscribe = (listener: () => void) => { this.listeners.add(listener); return () => { this.listeners.delete(listener); }; };
  private set(next: Partial<SessionState<A>>) {
    if (this.closed) return;
    this.state = Object.freeze({ ...this.state, ...next });
    for (const listener of this.listeners) listener();
  }
  resolve = async (): Promise<SessionState<A>> => {
    if (this.closed) throw new Error("Client closed");
    if (this.state.phase === "error") throw new Error(this.state.error ?? "Session unavailable");
    if (this.state.phase !== "loading") return this.state;
    await new Promise<void>((resolve, reject) => {
      const stop = this.subscribe(() => {
        if (this.closed || this.state.phase !== "loading") { stop(); resolve(); }
      });
      if (this.closed) { stop(); reject(new Error("Client closed")); }
      else if (!this.pending) void this.refresh();
    });
    if (this.closed) throw new Error("Client closed");
    if (this.getSnapshot().phase === "error") throw new Error(this.state.error ?? "Session unavailable");
    return this.state;
  };
  refresh = (): Promise<SessionState<A>> => {
    if (this.closed) return Promise.reject(new Error("Client closed"));
    if (this.pending) return this.pending;
    const epoch = this.state.epoch;
    const pending = (async () => {
      try {
        const account = await this.options.identity.fetch();
        if (!this.closed && epoch === this.state.epoch) await this.install(account);
      } catch (error) {
        if (!this.closed && epoch === this.state.epoch) this.fail(error);
      }
      return this.state;
    })().finally(() => { if (this.pending === pending) this.pending = undefined; });
    this.pending = pending;
    return pending;
  };
  /** Used only after a committed Identity acquisition or sign-out. Invalidates
   * earlier HTTP checks even when the new session belongs to the same identity. */
  replace(account: A | null) {
    if (this.closed) return Promise.resolve();
    const epoch = this.state.epoch + 1;
    const pending = Promise.resolve().then(async () => {
      if (!this.closed && epoch === this.state.epoch) await this.install(account);
      return this.state;
    }).finally(() => { if (this.pending === pending) this.pending = undefined; });
    this.pending = pending;
    this.clear();
    this.set({ epoch, account: null, phase: "loading", error: null });
    return pending.then(() => {});
  }
  private async install(account: A | null) {
    if (this.closed) return;
    if (account && !this.options.key(account)) throw new Error("Identity is missing its actor");
    const changed = (account ? this.options.key(account) : null) !== (this.state.account ? this.options.key(this.state.account) : null);
    if (changed) {
      this.clear();
      this.set({ epoch: this.state.epoch + 1, phase: "loading", account, error: null });
    }
    if (!account) { this.set({ account: null, phase: "anonymous", error: null }); return; }
    const epoch = this.state.epoch;
    this.set({ account, error: null, ...(this.state.phase === "error" ? { phase: "loading" as const } : {}) });
    try {
      if (!this.binding) {
        const binding = await this.options.create(account);
        if (this.closed || epoch !== this.state.epoch) { binding.free(); return; }
        this.binding = binding;
      }
      this.connect();
    } catch (error) { if (!this.closed && epoch === this.state.epoch) this.fail(error); }
  }
  private connect() {
    if (this.closed || this.socket || !this.binding) return;
    clearTimeout(this.timer); this.timer = undefined;
    const binding = this.binding, epoch = this.state.epoch;
    this.set({ connection: "connecting" });
    const socket = this.socket = this.transport.connect(id => binding.connect(id));
    const current = () => !this.closed && this.socket === socket && this.state.epoch === epoch;
    this.deadline = setTimeout(() => { if (current()) { this.fail(new Error("Connection timed out")); socket.close(); } }, this.options.timeout ?? 10000);
    socket.onmessage = event => {
      if (!current()) return;
      try {
        const frame = String(event.data), response = JSON.parse(frame);
        if (response.Failed !== undefined || response === "Detached") throw new Error(JSON.stringify(response));
        if (response.Attached) this.set({ connection: "connected" });
        if (this.calls.receive(frame)) return;
        this.apply(binding.receive(frame));
      } catch (error) { this.fail(error); socket.close(); }
    };
    socket.onclose = () => {
      if (!current()) return;
      clearTimeout(this.deadline);
      this.socket = undefined;
      this.calls.detached();
      this.set({ connection: "disconnected" });
      this.schedule();
    };
  }
  private schedule() {
    if (this.closed || this.timer) return;
    const delay = this.backoff; this.backoff = Math.min(delay * 2, 5000);
    this.timer = setTimeout(() => { this.timer = undefined; void this.refresh(); }, delay);
  }
  private fail(error: unknown) {
    this.set({ error: error instanceof Error ? error.message : String(error), ...(this.state.phase === "ready" ? {} : { phase: "error" as const }) });
    if (this.state.account) this.schedule();
  }
  private send(frame: string) {
    if (this.socket?.readyState !== 1) throw new Error("Disconnected");
    this.socket.send(frame);
  }
  private apply(raw: string) {
    const value = this.options.decode(raw);
    // A Reset during physical recovery is not an Identity decision. Keep the
    // last converged view until the replacement manifest or confirmed sign-out.
    if (value.ready || this.state.phase !== "ready") this.options.publish(value);
    for (const frame of value.send) this.send(frame);
    if (value.ready && this.state.connection === "connected") {
      clearTimeout(this.deadline); this.backoff = 250;
      this.set({ phase: "ready", error: null });
    }
    if (value.error && /InvalidBearer|StaleConnection/.test(value.error)) this.socket?.close();
  }
  mutate(work: (binding: B) => string) {
    if (!this.binding || this.state.phase !== "ready") throw new Error("Wait for the connection before editing");
    this.apply(work(this.binding));
  }
  invoke<T>(operation: string, input: unknown): Promise<T> { return this.calls.invoke<T>(operation, input); }
  private clear() {
    clearTimeout(this.timer); this.timer = undefined;
    clearTimeout(this.deadline);
    this.calls.close("Account session ended");
    if (this.socket) {
      this.socket.onopen = this.socket.onmessage = this.socket.onclose = this.socket.onerror = null;
      this.socket.close(); this.socket = undefined;
    }
    this.binding?.free(); this.binding = undefined;
    // A discarded binding has discarded its wire sequence and receipts. Never
    // reattach it to a retained logical lifetime, even for the same actor.
    this.transport = this.options.transport ?? new Transport();
    this.options.publish(null);
    this.set({ connection: "disconnected" });
  }
  close = () => {
    if (this.closed) return;
    this.clear(); this.closed = true;
    for (const listener of this.listeners) listener();
    this.listeners.clear();
  };
}

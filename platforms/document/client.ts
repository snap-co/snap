/** Invocation channels shared by application SDKs. A surviving logical connection
 * permits same-ID retries; a fresh lifetime must never replay unknown effects. */
type Event = { Accepted: { id: number } } | { Progress: { id: number; value: unknown } } | { Completed: { id: number; outcome: { Ok: unknown } | { Err: unknown } } };
type Pending = { frame: string; accepted: boolean; timer?: ReturnType<typeof setTimeout>; resolve(value: unknown): void; reject(error: Error): void; progress?(value: unknown): void };
export class Invocations {
  private pending = new Map<number, Pending>();
  private settled = new Set<number>();
  constructor(private encode: (operation: string, input: unknown) => string, private send: (frame: string) => void) {}
  invoke<T, P = unknown>(operation: string, input: unknown, progress?: (value: P) => void): Promise<T> {
    const frame = this.encode(operation, input);
    const id = JSON.parse(frame).Invoke.id as number;
    if (!Number.isSafeInteger(id) || id <= 0 || this.pending.has(id)) throw new Error("Invalid invocation ID");
    return new Promise<T>((resolve, reject) => {
      const call: Pending = { frame, accepted: false, resolve: value => resolve(value as T), reject, progress: progress as ((value: unknown) => void) | undefined };
      this.pending.set(id, call);
      this.transmit(id, call);
    });
  }
  private transmit(id: number, call: Pending) {
    clearTimeout(call.timer);
    try { this.send(call.frame); } catch { return; }
    if (!call.accepted) call.timer = setTimeout(() => { if (this.pending.get(id) === call) this.transmit(id, call); }, 2000);
  }
  /** Returns true only for frames owned by these application channels. */
  receive(frame: string): boolean {
    const response = JSON.parse(frame) as { Events?: Event[]; Attached?: { resumed: boolean }; Failed?: unknown };
    if (response.Failed !== undefined) { this.close(JSON.stringify(response.Failed)); return false; }
    if (response.Attached) {
      if (!response.Attached.resumed) this.close("Logical connection ended; prior outcomes are unknown");
      else for (const [id, call] of this.pending) this.transmit(id, call);
      return false;
    }
    if (!response.Events?.length) return false;
    const ids = response.Events.map(event => "Accepted" in event ? event.Accepted.id : "Progress" in event ? event.Progress.id : event.Completed.id);
    if (!ids.some(id => this.pending.has(id) || this.settled.has(id))) return false;
    if (!ids.every(id => this.pending.has(id) || this.settled.has(id))) throw new Error("Mixed invocation channels");
    for (const event of response.Events) {
      const id = "Accepted" in event ? event.Accepted.id : "Progress" in event ? event.Progress.id : event.Completed.id;
      if (this.settled.has(id)) continue;
      const call = this.pending.get(id)!;
      if ("Accepted" in event) { call.accepted = true; clearTimeout(call.timer); }
      else if ("Progress" in event) {
        if (!call.accepted) throw new Error("Progress before acceptance");
        call.progress?.(event.Progress.value);
      } else {
        const outcome = event.Completed.outcome;
        if ("Ok" in outcome && !call.accepted) throw new Error("Completion before acceptance");
        clearTimeout(call.timer); this.pending.delete(id); this.settled.add(id);
        if ("Err" in outcome) call.reject(new Error(JSON.stringify(outcome.Err)));
        else call.resolve(outcome.Ok);
      }
    }
    return true;
  }
  detached() { for (const call of this.pending.values()) clearTimeout(call.timer); }
  close(reason = "Client closed; outstanding outcomes are unknown") {
    for (const call of this.pending.values()) { clearTimeout(call.timer); call.reject(new Error(reason)); }
    this.pending.clear();
    this.settled.clear();
  }
}

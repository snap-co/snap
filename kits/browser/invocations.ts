/** Application invocation correlation only. Physical loss or missing acceptance
 * leaves effects unknown; recovery belongs to the operation, never hidden retries. */
type Event = { Accepted: { id: number } } | { Progress: { id: number; value: unknown } } | { Completed: { id: number; outcome: { Ok: unknown } | { Err: unknown } } };
type Pending = { accepted: boolean; timer?: ReturnType<typeof setTimeout>; resolve(value: unknown): void; reject(error: Error): void; progress?(value: unknown): void };
export class Invocations {
  private pending = new Map<number, Pending>();
  private settled = new Set<number>();
  constructor(private encode: (operation: string, input: unknown) => string, private send: (frame: string) => void) {}
  invoke<T, P = unknown>(operation: string, input: unknown, progress?: (value: P) => void): Promise<T> {
    const frame = this.encode(operation, input);
    const id = JSON.parse(frame).Invoke.id as number;
    if (!Number.isSafeInteger(id) || id <= 0 || this.pending.has(id)) throw new Error("Invalid invocation ID");
    return new Promise<T>((resolve, reject) => {
      const call: Pending = { accepted: false, resolve: value => resolve(value as T), reject, progress: progress as ((value: unknown) => void) | undefined };
      this.pending.set(id, call);
      try { this.send(frame); } catch { this.abandon(id, call, "Invocation delivery failed; outcome is unknown"); return; }
      if (!call.accepted && this.pending.get(id) === call) {
        call.timer = setTimeout(() => this.abandon(id, call, "Invocation acceptance timed out; outcome is unknown"), 2000);
      }
    });
  }
  private abandon(id: number, call: Pending, reason: string) {
    if (this.pending.get(id) !== call) return;
    clearTimeout(call.timer);
    this.pending.delete(id); this.settled.add(id);
    call.reject(new Error(reason));
  }
  /** Returns true only for frames owned by these application channels. */
  receive(frame: string): boolean {
    const response = JSON.parse(frame) as { Events?: Event[]; Attached?: { resumed: boolean }; Failed?: unknown };
    if (response.Failed !== undefined) { this.close(JSON.stringify(response.Failed)); return false; }
    if (response.Attached) {
      this.close("Connection replaced; outstanding outcomes are unknown");
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
  detached() { this.close("Physical connection lost; outstanding outcomes are unknown"); }
  close(reason = "Client closed; outstanding outcomes are unknown") {
    for (const call of this.pending.values()) { clearTimeout(call.timer); call.reject(new Error(reason)); }
    this.pending.clear();
    this.settled.clear();
  }
}

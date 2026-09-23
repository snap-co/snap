// SDK assertions shared across native Rust, WASM and the reference TypeScript client.
export interface HealthyClient {
  readonly health: { up(): Promise<{ status: "OK" }> };
  close(): Promise<void>;
}

export async function healthyContract(client: HealthyClient): Promise<void> {
  const report = await client.health.up();
  if (report.status !== "OK")
    throw new Error(`Expected healthy status, received ${report.status}`);
}

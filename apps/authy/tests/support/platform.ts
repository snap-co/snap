// Direct invocations default to native; the CLI always supplies an explicit host.
const selected = process.env.SNAP_TEST_PLATFORM ?? "native";
if (!["native", "workers", "browser"].includes(selected)) {
  throw new Error(`Unsupported Authy integration platform: ${selected}`);
}
export const hosts: ("native" | "workers")[] = selected === "browser"
  ? ["native", "workers"] : [selected as "native" | "workers"];

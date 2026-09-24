import { spawn } from "node:child_process";
import { resolve } from "node:path";

/** Owns one real host on an ephemeral port; never replaces development listeners. */
export async function startServer(
  options: { executable?: string; web?: boolean; dev?: boolean; project?: string; env?: NodeJS.ProcessEnv; freshBuild?: boolean } = {},
) {
  const root = resolve(import.meta.dirname, "../..");
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    ...options.env,
    SNAP_ADDR: "127.0.0.1:0",
    SNAP_BACKEND_ADDR: "127.0.0.1:0",
    SNAP_BUILD: "healthy-smoke",
  };
  delete env.SNAP_WEB_DIR;
  if (options.freshBuild) delete env.SNAP_BUILD;
  let logs = "";
  let backendUrl: string | undefined;
  if (options.web) env.SNAP_WEB_DIR = resolve(root, "dist/web");
  const child = spawn(
    options.executable ??
      resolve(
        root,
        options.dev ? "target/debug/snap" : "target/debug/healthy",
      ),
    options.dev ? ["dev", ...(options.project ? [options.project] : [])] : [],
    {
      env,
      cwd: options.dev && !options.project ? resolve(root, "apps/healthy") : root,
      stdio: ["ignore", "ignore", "pipe"],
    },
  );
  const exited = new Promise<void>((done) => {
    child.once("exit", () => done());
    child.once("error", () => done());
  });
  async function close() {
    if (
      child.exitCode !== null ||
      child.signalCode !== null ||
      child.pid === undefined
    )
      return;
    child.kill("SIGTERM");
    const kill = setTimeout(
      () => child.kill("SIGKILL"),
      options.dev ? 8_000 : 3_000,
    );
    try {
      await exited;
    } finally {
      clearTimeout(kill);
    }
  }
  try {
    const baseUrl = await new Promise<string>((done, fail) => {
      let settled = false;
      const timeout = setTimeout(
        () => finish(new Error(`Host readiness timeout: ${logs}`)),
        options.dev ? 120_000 : 10_000,
      );
      function finish(error?: Error, url?: string) {
        if (settled) return;
        settled = true;
        clearTimeout(timeout);
        child.off("exit", earlyExit);
        if (error) fail(error);
        else done(url!);
      }
      const earlyExit = () =>
        finish(new Error(`Host exited before readiness: ${logs}`));
      child.once("error", (error) => finish(error));
      child.once("exit", earlyExit);
      child.stderr.on("data", (chunk: Buffer) => {
        logs += chunk.toString();
        backendUrl = logs.match(/Backend ready at (http:\/\/[^\s]+)/)?.[1] ?? backendUrl;
        const match = logs.match(/^listening on (http:\/\/[^\s]+)\r?\n/m);
        if (match) finish(undefined, match[1]);
      });
    });
    return { baseUrl, backendUrl, close, logs: () => logs };
  } catch (error) {
    await close();
    throw error;
  }
}

export async function deadline<T>(
  promise: Promise<T>,
  milliseconds: number,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timer = setTimeout(
          () => reject(new Error(`Timed out after ${milliseconds}ms`)),
          milliseconds,
        );
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

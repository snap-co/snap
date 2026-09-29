import { test as base, expect, type Page } from "@playwright/test";
import { spawn, execFileSync } from "node:child_process";
import { resolve } from "node:path";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import { deployment } from "../../../tests/support/deployment";

const test = base.extend<{ server: string }>({
  server: async ({}, use) => {
    const root = resolve(import.meta.dirname, "../../..");
    await mkdir(resolve(root, ".tmp"), { recursive: true });
    const directory = await mkdtemp(resolve(root, ".tmp/testy-web-"));
    const database = resolve(directory, "identity.sqlite");
    execFileSync(resolve(root, "target/debug/snap"), ["migrate", "--database", database,
      "--migrations", "crates/identity/migrations"], { cwd: root });
    const setup = await deployment(directory, { host: { mode: "development", listen: "127.0.0.1:0", data_dir: directory, database: "identity.sqlite", web_dir: `${root}/apps/testy/dist/development/web` }, app: {} });
    const child = spawn(resolve(root, "target/debug/testy-web"), ["--config", setup.path], {
      cwd: root,
      env: setup.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let logs = "";
    try {
      const url = await new Promise<string>((resolve, reject) => {
        const timer = setTimeout(
          () => reject(new Error(`Host readiness timed out: ${logs}`)),
          10_000,
        );
        child.once("error", (error) => {
          clearTimeout(timer);
          reject(error);
        });
        child.once("exit", (code) => {
          clearTimeout(timer);
          reject(new Error(`Host exited ${code}: ${logs}`));
        });
        child.stderr.on("data", (bytes) => {
          logs += bytes;
        });
        child.stdout.on("data", (bytes) => {
          logs += bytes;
          const match = /Testy (http:\/\/[^\s]+)/.exec(logs);
          if (match) {
            clearTimeout(timer);
            resolve(match[1]);
          }
        });
      });
      await use(url);
    } finally {
      if (child.exitCode === null && child.signalCode === null) {
        const stopped = new Promise<void>((resolve) =>
          child.once("exit", () => resolve()),
        );
        child.kill();
        await stopped;
      }
      await rm(directory, { recursive: true, force: true });
    }
  },
});

async function login(page: Page, enroll = true) {
  await page.getByLabel("Email", { exact: true }).fill("alice@example.com");
  await page.getByLabel("Password", { exact: true }).fill("testy-password1");
  await page.getByRole("button", { name: enroll ? "Create account" : "Sign in", exact: true }).click();
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
}

test("login sessions isolate calculators and sign-out closes every connection of that session", async ({ page, context, server }) => {
  await page.goto(`${server}/calc`);
  await expect(page.getByRole("button", { name: "+", exact: true })).toBeDisabled();
  await login(page);
  await page.getByLabel("Operand", { exact: true }).fill("12");
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("12");
  const second = await context.newPage();
  await second.goto(`${server}/calc`);
  await login(second, false);
  await expect(second.getByTestId("accumulator")).toHaveText("0");
  await second.getByLabel("Operand", { exact: true }).fill("7");
  await second.getByRole("button", { name: "+", exact: true }).click();
  await expect(second.getByTestId("accumulator")).toHaveText("7");
  const token = await page.evaluate(() => sessionStorage.getItem("testy.session")!);
  const sibling = await context.newPage();
  await sibling.addInitScript(token => sessionStorage.setItem("testy.session", token), token);
  await sibling.goto(`${server}/calc`);
  await expect(sibling.getByText("Connected", { exact: true })).toBeVisible();
  await expect(sibling.getByTestId("accumulator")).toHaveText("0");
  await page.getByRole("button", { name: "Sign out", exact: true }).click();
  await expect(page.getByRole("button", { name: "Sign in", exact: true })).toBeVisible();
  await expect(page.getByTestId("accumulator")).toHaveText("0");
  await expect(sibling.getByText("Disconnected", { exact: true })).toBeVisible();
  await second.getByRole("button", { name: "Refresh", exact: true }).click();
  await expect(second.getByTestId("accumulator")).toHaveText("7");
  const diagnostics = await (await page.request.get(`${server}/__dev`)).text();
  expect(diagnostics).not.toContain(token);
  expect(diagnostics).not.toContain("testy-password1");
  expect(await page.locator(".wire").textContent()).not.toContain(token);
  expect(await page.locator(".wire").textContent()).not.toContain("testy-password1");
});

test("WebSocket envelopes and attachment ownership work without the SDK", async ({
  page,
  server,
}) => {
  await page.goto(server);
  const observations = await page.evaluate(async () => {
    async function open() {
      const ws = new WebSocket(
        `${location.origin.replace("http", "ws")}/transport`,
      );
      await new Promise<void>((resolve, reject) => {
        ws.onopen = () => resolve();
        ws.onerror = () => reject(new Error("connect"));
      });
      return ws;
    }
    function exchange(ws: WebSocket, command: unknown) {
      return new Promise<unknown[]>((resolve, reject) => {
        const frames: unknown[] = [];
        ws.onclose = () => reject(new Error("unexpected close"));
        ws.onmessage = ({ data }) => {
          const response = JSON.parse(data);
          frames.push(response);
          if (
            !response.Events ||
            response.Events.some((event: any) => event.Completed)
          )
            resolve(frames);
        };
        ws.send(JSON.stringify(command));
      });
    }
    const first = await open();
    const second = await open();
    const health = await exchange(first, {
      Request: {
        bearer: null,
        invocation: { id: 1, operation: "health.up", input: null },
      },
    });
    const enrollment = await exchange(first, { Request: { bearer: null,
      invocation: { id: 2, operation: "identity.enroll", input: { email: "raw@example.com", password: "password1" } } } });
    const token = (enrollment.at(-1) as any).Events[0].Completed.outcome.Ok.bearer;
    const attach = {
      Connect: {
        bearer: token,
        client_id: "raw-frame-client",
      },
    };
    const attached = await exchange(first, attach);
    const occupied = await exchange(second, attach);
    await new Promise<void>((resolve) => {
      first.onclose = () => resolve();
      first.close();
    });
    const resumed = await exchange(second, attach);
    await exchange(second, "Close");
    second.close();
    return { health, attached, occupied, resumed };
  });
  expect(observations).toEqual({
    health: [
      { Events: [{ Accepted: { id: 1 } }] },
      { Events: [{ Completed: { id: 1, outcome: { Ok: { status: "OK" } } } }] },
    ],
    attached: [{ Attached: { resumed: false } }],
    occupied: [{ Failed: "Occupied" }],
    resumed: [{ Attached: { resumed: false } }],
  });
});

test("launcher, health, calculator, reload and explicit close", async ({
  page,
  server,
}) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(server);
  await page
    .getByRole("link", { name: "Healthy Check the connection" })
    .click();
  await page.getByRole("button", { name: "Check health" }).click();
  await expect(page.getByText("OK", { exact: true })).toBeVisible();
  await page.getByRole("link", { name: "All apps" }).click();
  await page
    .getByRole("link", { name: "Calculator State over transport" })
    .click();
  await login(page);
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await page.getByLabel("Operand", { exact: true }).fill("12");
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("12");
  await page.reload();
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await expect(page.getByTestId("accumulator")).toHaveText("0");
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await page.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await page.getByLabel("Operand", { exact: true }).fill("3");
  await page.getByRole("button", { name: "×", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("0");
  await page.getByRole("button", { name: "Close calculator" }).click();
  await page.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("0");
  // Preserve values above JavaScript's exact integer range through both carriers.
  await page.getByLabel("Operand", { exact: true }).fill("9007199254740993");
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("9007199254740993");
  expect(errors).toEqual([]);
});

test("agent control steps a live browser request and restores its state", async ({
  page,
  request,
  server,
}) => {
  expect((await page.goto(`${server}/calc`))?.status()).toBe(200);
  await login(page);
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  const control = async (body: object) => {
    const response = await request.post(`${server}/__dev`, { data: body });
    expect(response.ok(), await response.text()).toBeTruthy();
    return response.json();
  };
  await control({ action: "snapshot" });
  await control({ action: "breakpoint", enabled: true });
  await page.getByRole("button", { name: "Checked +", exact: true }).click();
  await expect
    .poll(
      async () =>
        (await (await request.get(`${server}/__dev`)).json()).active?.accepted,
    )
    .toBe(true);
  let state = await control({ action: "step" });
  expect(state.active.waiting).toBe("testy.calculator.ceiling");
  expect(state.states[0].state.accumulator).toBe(0);
  const bad = await request.post(`${server}/__dev`, {
    data: { action: "restore" },
  });
  expect(bad.status()).toBe(409);
  await control({
    action: "supply",
    ticket: state.active.ticket,
    key: state.active.waiting,
    value: 1000,
  });
  await control({ action: "breakpoint", enabled: false });
  await control({ action: "step" });
  await control({ action: "mode", manual: false });
  await expect(page.getByTestId("accumulator")).toHaveText("10");
  await expect(
    page.getByRole("button", { name: "Refresh", exact: true }),
  ).toBeEnabled();
  await control({ action: "restore" });
  await control({ action: "replace", program: "double-add" });
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("20");
  expect(
    (
      await request.post(`${server}/__dev`, {
        headers: { origin: "https://other.example" },
        data: { action: "step" },
      })
    ).status(),
  ).toBe(403);
});

test("execution desk displays and supplies exact i64 dependencies", async ({
  page,
  server,
}) => {
  const sent: string[] = [];
  page.on("websocket", (socket) => {
    if (socket.url().endsWith("/__dev/ws"))
      socket.on("framesent", (frame) => sent.push(String(frame.payload)));
  });
  await page.goto(`${server}/calc`);
  await login(page);
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await page.getByText("Committed records", { exact: true }).click();
  await page.locator("summary").filter({ hasText: "Execution trace" }).click();
  const values = [
    "9007199254740993",
    "9223372036854775807",
    "-9223372036854775808",
  ];
  for (const [index, value] of values.entries()) {
    if (index) {
      await page.getByRole("button", { name: "Close calculator" }).click();
      await page
        .getByRole("button", { name: "Reconnect", exact: true })
        .click();
      await expect(page.getByText("Connected", { exact: true })).toBeVisible();
    }
    await page.getByLabel("Operand", { exact: true }).fill(value);
    await page.getByRole("button", { name: "+", exact: true }).click();
    await expect(page.getByTestId("accumulator")).toHaveText(value);
    await expect(page.getByTestId("host-state")).toContainText(
      `"accumulator": ${value}`,
    );
    await page.getByLabel("Break after acceptance").click();
    await expect(page.getByLabel("Break after acceptance")).toBeChecked();
    await page.getByLabel("Operand", { exact: true }).fill("0");
    await page.getByRole("button", { name: "Checked +", exact: true }).click();
    await expect(page.locator(".execution-status")).toContainText(
      "ready for attempt",
    );
    await page.getByRole("button", { name: "Step once" }).click();
    // Invalid JSON is rejected locally and leaves the outstanding dependency intact.
    await page.getByLabel("Dependency value").fill(`${value} trailing`);
    await page.getByRole("button", { name: "Supply input" }).click();
    await expect(page.getByRole("alert")).toBeVisible();
    await page.getByLabel("Dependency value").fill(value);
    await page.getByRole("button", { name: "Supply input" }).click();
    await expect(page.getByLabel("Dependency value")).toHaveCount(0);
    expect(
      sent.filter((frame) => frame.includes('"action":"supply"')).at(-1),
    ).toContain(`"value":${value}`);
    await page.getByLabel("Break after acceptance").click();
    await expect(page.getByLabel("Break after acceptance")).not.toBeChecked();
    await page.getByRole("button", { name: "Step once" }).click();
    await page.getByRole("button", { name: "Run", exact: true }).click();
    await expect(
      page.getByRole("button", { name: "Refresh", exact: true }),
    ).toBeEnabled();
    await expect(page.getByRole("alert")).toHaveCount(0);
    await expect(
      page.getByRole("heading", { name: "History 2", exact: true }),
    ).toBeVisible();
    await expect(page.getByTestId("accumulator")).toHaveText(value);
    await expect(page.getByTestId("host-trace")).toContainText(
      `"Ok": ${value}`,
    );
  }
});

test("debugger pushes changes, correlates commands and has no application lifecycle", async ({
  page,
  server,
}) => {
  await page.goto(server);
  const result = await page.evaluate(async () => {
    async function debuggerSocket() {
      const socket = new WebSocket(
        `${location.origin.replace("http", "ws")}/__dev/ws`,
      );
      const responses = new Map<string, (frame: any) => void>();
      const reports: any[] = [];
      let wake: (() => void) | undefined;
      socket.onmessage = ({ data }) => {
        const frame = JSON.parse(data);
        if (frame.type === "state") {
          reports.push(frame);
          wake?.();
        } else {
          const resolve = responses.get(frame.id);
          responses.delete(frame.id);
          resolve?.(frame);
        }
      };
      async function state(predicate: (state: any) => boolean) {
        while (!reports.length || !predicate(reports.at(-1).state))
          await new Promise<void>((resolve) => {
            wake = resolve;
          });
        wake = undefined;
        return reports.at(-1);
      }
      await state(() => true);
      return {
        socket,
        reports,
        state,
        command(id: string, control: object) {
          return new Promise<any>((resolve) => {
            responses.set(id, resolve);
            socket.send(JSON.stringify({ id, control }));
          });
        },
      };
    }
    const first = await debuggerSocket();
    const second = await debuggerSocket();
    const initial = first.reports[0];
    const commands = await Promise.all([
      first.command("hold", { action: "mode", manual: true }),
      first.command("invalid", { action: "does_not_exist" }),
      first.command("save", { action: "snapshot" }),
    ]);
    const observed = await second.state(
      (state) => state.manual && state.snapshot,
    );
    await new Promise<void>((resolve) => {
      first.socket.onclose = () => resolve();
      first.socket.close();
    });
    // An HTTP change also reaches subscribers, without a socket command or poll.
    await fetch("/__dev", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ action: "breakpoint", enabled: true }),
    });
    await second.state((state) => state.breakpoint);
    const reconnected = await debuggerSocket();
    const resumed = reconnected.reports[0];
    await reconnected.command("run", { action: "mode", manual: false });
    await second.state((state) => !state.manual);
    second.socket.close();
    reconnected.socket.close();
    return { initial, commands, observed, resumed };
  });
  expect(result.initial.type).toBe("state");
  expect(result.initial.state.peers).toEqual([]);
  expect(result.commands.map((frame) => frame.id)).toEqual([
    "hold",
    "invalid",
    "save",
  ]);
  expect(result.commands[0].result.manual).toBe(true);
  expect(result.commands[1].error).toContain("unknown variant");
  expect(result.commands[2].result.snapshot).toBe(true);
  expect(BigInt(result.observed.revision)).toBeGreaterThan(
    BigInt(result.initial.revision),
  );
  expect(result.resumed.state).toMatchObject({
    manual: true,
    snapshot: true,
    breakpoint: true,
    peers: [],
    states: [],
  });
});

test("execution desk reconnects independently and is silent while idle", async ({
  page,
  request,
  server,
}) => {
  const polls: string[] = [];
  let debuggerFrames = 0;
  let appSockets = 0;
  page.on("request", (request) => {
    if (request.url().endsWith("/__dev")) polls.push(request.method());
  });
  page.on("websocket", (socket) => {
    if (socket.url().endsWith("/transport")) appSockets++;
    if (socket.url().endsWith("/__dev/ws"))
      socket.on("framereceived", () => debuggerFrames++);
  });
  await page.addInitScript(() => {
    const Native = window.WebSocket;
    (window as any).debuggers = [];
    window.WebSocket = class extends Native {
      constructor(url: string | URL, protocols?: string | string[]) {
        super(url, protocols);
        if (String(url).endsWith("/__dev/ws"))
          (window as any).debuggers.push(this);
      }
    };
  });
  await page.goto(`${server}/calc`);
  await login(page);
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await expect(page.getByText("Debugger: Live", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("10");
  await page.getByText("Committed records", { exact: true }).click();
  await expect(page.getByTestId("host-state")).toContainText(
    '"accumulator": 10',
  );
  await page.getByRole("button", { name: "Hold", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Run", exact: true }),
  ).toBeVisible();
  const before = await (await request.get(`${server}/__dev`)).json();
  await page.evaluate(() => (window as any).debuggers.at(-1).close());
  await expect
    .poll(() => page.evaluate(() => (window as any).debuggers.length))
    .toBe(2);
  await expect(page.getByText("Debugger: Live", { exact: true })).toBeVisible();
  expect(await (await request.get(`${server}/__dev`)).json()).toEqual(before);
  expect(appSockets).toBe(2); // Initial anonymous channel plus explicit login channel.
  // Advance browser timers without waiting: old 250ms HTTP polling would fire.
  await page.clock.install();
  const framesBefore = debuggerFrames;
  await page.clock.runFor(1100);
  expect(polls).toEqual([]);
  expect(debuggerFrames).toBe(framesBefore);
  await page.getByRole("button", { name: "Run", exact: true }).click();
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("20");
});

test("a lost debugger response fails the command without replaying the step", async ({
  page,
  request,
  server,
}) => {
  let dropped = false;
  let steps = 0;
  await page.routeWebSocket("**/__dev/ws", (socket) => {
    const upstream = socket.connectToServer();
    let stepID: string | undefined;
    socket.onMessage((message) => {
      const frame = JSON.parse(String(message));
      if (frame.control.action === "step") {
        steps++;
        stepID = frame.id;
      }
      upstream.send(message);
    });
    upstream.onMessage((message) => {
      const frame = JSON.parse(String(message));
      if (!dropped && frame.type === "result" && frame.id === stepID) {
        dropped = true;
        socket.close();
        upstream.close();
      } else socket.send(message);
    });
  });
  await page.goto(`${server}/calc`);
  await login(page);
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Hold", exact: true }).click();
  await expect(
    page.getByRole("button", { name: "Run", exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.locator(".execution-status")).toContainText(
    "1 queued operations",
  );
  await page.getByRole("button", { name: "Step once" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "command outcome may be unknown",
  );
  await expect(page.getByText("Debugger: Live", { exact: true })).toBeVisible();
  await expect(page.locator(".execution-status")).toContainText(
    "ready for attempt",
  );
  const state = await (await request.get(`${server}/__dev`)).json();
  expect(state.states[0].state.accumulator).toBe(0);
  expect(state.active.accepted).toBe(true);
  expect(steps).toBe(1);
  await page.getByRole("button", { name: "Step once" }).click();
  await page.getByRole("button", { name: "Run", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("10");
});

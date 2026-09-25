import { test as base, expect } from "@playwright/test";
import { spawn } from "node:child_process";
import { resolve } from "node:path";

const test = base.extend<{ server: string }>({
  server: async ({}, use) => {
    const root = resolve(import.meta.dirname, "../../..");
    const child = spawn(resolve(root, "target/debug/testy-web"), [], {
      cwd: root,
      env: { ...process.env, TESTY_WEB_ADDR: "127.0.0.1:0" },
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
    }
  },
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
    const attach = {
      Connect: {
        bearer: "testy-private-fixture-token",
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
    resumed: [{ Attached: { resumed: true } }],
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
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await page.getByLabel("Operand", { exact: true }).fill("12");
  await page.getByRole("button", { name: "+", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("12");
  await page.reload();
  await expect(page.getByTestId("accumulator")).toHaveText("12");
  await page.getByRole("button", { name: "Disconnect", exact: true }).click();
  await page.getByRole("button", { name: "Reconnect", exact: true }).click();
  await expect(page.getByText("Connected", { exact: true })).toBeVisible();
  await page.getByLabel("Operand", { exact: true }).fill("3");
  await page.getByRole("button", { name: "×", exact: true }).click();
  await expect(page.getByTestId("accumulator")).toHaveText("36");
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

import init, { Client } from "@snap/wasm";
import type { WebChannel } from "./channel";

/** Testy uses explicit SDK connections, not the Document session runtime. */
export class TestyClient {
  readonly ready = init({ module_or_path: "/bindings/testy_wasm_bg.wasm" });
  create(channel: WebChannel) { return new Client(channel); }
}

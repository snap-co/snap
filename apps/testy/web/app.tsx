import React from "react";
import { createRoot } from "react-dom/client";
import init from "../.snap/web/bindings/testy_wasm.js";
import { App } from "./screens";

await init({ module_or_path: "/bindings/testy_wasm_bg.wasm" });
createRoot(document.getElementById("root")!).render(<App />);

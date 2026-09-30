import { mount } from "../../../kits/react/host";
import { Factorio } from "../client";
import { createAppRouter } from "./routes";
import "./style.css";

const client = new Factorio(location.origin);
const router = createAppRouter(client);
const dispose = mount({ router, runtime: client.runtime, element: document.getElementById("root")!, dispose: () => client.close() });
if (import.meta.hot) import.meta.hot.dispose(dispose);

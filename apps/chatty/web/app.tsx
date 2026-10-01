import { mount } from "../../../kits/react/host";
import { Chatty } from "./client";
import { createAppRouter } from "./routes";
import "./style.css";

const client = new Chatty();
const router = createAppRouter(client);
const dispose = mount({ router, runtime: client.runtime, element: document.getElementById("root")!, dispose: () => client.close() });
if (import.meta.hot) import.meta.hot.dispose(dispose);

import { mount } from "../../../kits/react/host";
import { TestyClient } from "./client";
import { createAppRouter } from "./routes";
import "./style.css";

const client = new TestyClient();
const router = createAppRouter(client);
const dispose = mount({ router, element: document.getElementById("root")! });
if (import.meta.hot) import.meta.hot.dispose(dispose);

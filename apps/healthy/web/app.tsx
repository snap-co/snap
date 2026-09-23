import {
  startHealthy,
  type HealthyClient,
} from "../../../clients/typescript/src";
import type { Application } from "../../../clients/react/host";
import { HealthMonitor } from "./health-monitor";
import "./style.css";

export default {
  start: startHealthy,
  View: HealthMonitor,
} satisfies Application<HealthyClient>;

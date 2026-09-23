// Application selection is supplied by the build, not a per-app init file.
import application from "snap:application";
import { run } from "./host";

void run(application).catch((error: unknown) => {
  console.error(error);
  const root = document.getElementById("root");
  if (root) {
    root.setAttribute("role", "alert");
    root.textContent = "Healthy could not start. Reload to retry.";
  }
});

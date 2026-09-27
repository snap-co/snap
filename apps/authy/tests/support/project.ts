import { applicationProject } from "../../../../tests/adapters/project";

export function authyProject() {
  return applicationProject("authy", "apps/authy", true, '"../../clients/typescript/src"');
}

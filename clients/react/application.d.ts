declare module "snap:application" {
  const application: typeof import("../../apps/healthy/web/app").default;
  export default application;
}

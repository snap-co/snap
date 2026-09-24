declare module "snap:application" {
  const application: typeof import("./app").default;
  export default application;
}

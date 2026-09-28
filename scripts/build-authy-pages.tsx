import { renderToStaticMarkup } from "react-dom/server";
import { AuthErrorPage, ConsentPage, LogoutPage, Permission } from "../apps/authy/web/auth-ui";

/** One set of React components serves interactive and script-free auth pages. */
export async function buildAuthyPages(outdir: string) {
  const templates = {
    consent: renderToStaticMarkup(<ConsentPage />),
    logout: renderToStaticMarkup(<LogoutPage />),
    error: renderToStaticMarkup(<AuthErrorPage />),
    permission: renderToStaticMarkup(<Permission title="{{title}}" description="{{description}}" />),
  };
  await Bun.write(`${outdir}/auth-pages.json`, JSON.stringify(templates));
  // This is a build artifact from the same source imported by the React app.
  await Bun.write(`${outdir}/style.css`, Bun.file("apps/authy/web/style.css"));
}

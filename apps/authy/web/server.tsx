import { renderToStaticMarkup } from "react-dom/server";
import { readFile } from "node:fs/promises";
import { AuthErrorPage, ConsentPage, LogoutPage, Permission } from "./auth-ui";

// Framework server-assets convention. These templates share the browser UI.
export default async function assets() {
  return {
    "auth-pages.json": JSON.stringify({
      consent: renderToStaticMarkup(<ConsentPage />),
      logout: renderToStaticMarkup(<LogoutPage />),
      error: renderToStaticMarkup(<AuthErrorPage />),
      permission: renderToStaticMarkup(<Permission title="{{title}}" description="{{description}}" />),
    }),
    "style.css": await readFile(new URL("./style.css", import.meta.url), "utf8"),
  };
}

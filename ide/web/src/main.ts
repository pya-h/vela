// Vela Studio's page: finds the session token, loads the analysis module and
// the workspace, and shows the workbench (or explains what went wrong).

import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/600.css";
import "./styles/base.css";
import "./styles/workbench.css";
import "./styles/editor.css";

import { VelaAnalysis } from "./analysis/bridge";
import { ApiError, StudioApi } from "./api/client";
import { takeToken } from "./api/session";
import { Analyzer } from "./editor/vela";
import { h } from "./workbench/dom";
import { toast } from "./workbench/toast";
import { Workbench } from "./workbench/workbench";

const wasmUrl = new URL("../wasm/vela_analysis.wasm", import.meta.url);

/** The nonce the server put in the page, allowing CodeMirror's styles. */
function cspNonce(): string {
  const value = document.querySelector<HTMLMetaElement>('meta[name="csp-nonce"]')?.content ?? "";
  // Unreplaced when served by the Vite dev server, which sends no policy.
  return value.startsWith("__") ? "" : value;
}

function screen(title: string, ...lines: (string | HTMLElement)[]): void {
  document.body.replaceChildren(
    h(
      "main",
      { class: "screen" },
      h("div", { class: "screen-mark", attrs: { "aria-hidden": "true" }, text: "◆" }),
      h("h1", { text: title }),
      ...lines.map((line) => (typeof line === "string" ? h("p", { text: line }) : line)),
    ),
  );
}

function needsToken(): void {
  screen(
    "Open Vela Studio from its link",
    "This page needs the address that vela-studio printed when it started, which includes a secret token.",
    h("pre", { text: "vela-studio path/to/workspace" }),
  );
}

async function start(): Promise<void> {
  // Anything blocked by the page's security policy is a bug worth seeing.
  document.addEventListener("securitypolicyviolation", (event) => {
    console.error(`Content-Security-Policy blocked ${event.blockedURI || "inline code"} (${event.violatedDirective})`);
  });
  window.addEventListener("unhandledrejection", (event) => {
    console.error(event.reason);
    toast(`Something went wrong: ${event.reason instanceof Error ? event.reason.message : String(event.reason)}`, "error");
  });

  const token = takeToken(window.location, window.history, window.sessionStorage);
  if (!token) {
    needsToken();
    return;
  }
  document.body.replaceChildren(h("div", { class: "loading", attrs: { role: "status", "aria-label": "Loading" } }, h("span", { text: "◆" })));

  const api = new StudioApi(token);
  try {
    const [info, tree, analysis] = await Promise.all([api.info(), api.files(), VelaAnalysis.load(fetch(wasmUrl))]);
    const analyzer = new Analyzer(analysis, analysis.language());
    document.title = `${info.workspace.name} · Vela Studio`;
    const workbench = new Workbench(api, info, analyzer, cspNonce(), tree);
    document.body.replaceChildren(workbench.element);
  } catch (error) {
    if (error instanceof ApiError && error.unauthorized) {
      needsToken();
    } else {
      screen("Vela Studio couldn't start", error instanceof Error ? error.message : String(error), "Check that vela-studio is still running, then reload.");
    }
  }
}

void start();

import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/600.css";
import "./styles/base.css";
import "./styles/workbench.css";
import "./styles/editor.css";
import "./styles/panel.css";

import { VelaAnalysis } from "./analysis/bridge";
import { ApiError, StudioApi } from "./api/client";
import { nonceStyleElements } from "./csp";
import { obtainToken } from "./api/session";
import { Analyzer } from "./editor/vela";
import { h } from "./workbench/dom";
import { applyInitialTheme } from "./workbench/theme";
import { toast } from "./workbench/toast";
import { Workbench } from "./workbench/workbench";

const wasmUrl = new URL("../wasm/vela_analysis.wasm", import.meta.url);

function cspNonce(): string {
  const value = document.querySelector<HTMLMetaElement>('meta[name="csp-nonce"]')?.content ?? "";
  // Unreplaced when served by Vite dev server without CSP headers.
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

function needsToken(refusal: string | null): void {
  const reason = refusal ? `${refusal.charAt(0).toUpperCase()}${refusal.slice(1)}.` : "This page needs a link from vela-studio.";
  screen(
    refusal ? "This link no longer works" : "Open Vela Studio from its link",
    `${reason} Open the address that vela-studio printed when it started, which includes a secret token.`,
    h("pre", { text: "vela-studio path/to/workspace" }),
  );
}

function sessionStore(): Storage | null {
  try {
    return window.sessionStorage;
  } catch {
    return null;
  }
}

async function start(): Promise<void> {
  // Apply before first render to avoid a wrong-theme flash.
  applyInitialTheme();
  nonceStyleElements(cspNonce());
  document.addEventListener("securitypolicyviolation", (event) => {
    console.error(`Content-Security-Policy blocked ${event.blockedURI || "inline code"} (${event.violatedDirective})`);
  });
  window.addEventListener("unhandledrejection", (event) => {
    console.error(event.reason);
    toast(`Something went wrong: ${event.reason instanceof Error ? event.reason.message : String(event.reason)}`, "error");
  });

  let refusal: string | null = null;
  const token = await obtainToken(window.location, window.history, sessionStore(), (input, init) => fetch(input, init), (reason) => {
    refusal = reason;
  });
  if (!token) {
    needsToken(refusal);
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
      needsToken(null);
    } else {
      screen("Vela Studio couldn't start", error instanceof Error ? error.message : String(error), "Check that vela-studio is still running, then reload.");
    }
  }
}

void start();

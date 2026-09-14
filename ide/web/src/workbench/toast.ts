// Short notifications in the corner, announced to screen readers.

import { h, iconButton } from "./dom";
import { icon } from "./icons";

export type ToastKind = "info" | "success" | "error";

let container: HTMLElement | null = null;

export function toast(message: string, kind: ToastKind = "info", timeout = kind === "error" ? 8000 : 4000): void {
  if (!container) {
    container = h("div", { class: "toasts", attrs: { role: "status", "aria-live": "polite" } });
    document.body.append(container);
  }
  const dismiss = () => {
    element.classList.add("leaving");
    setTimeout(() => element.remove(), 200);
  };
  const element = h(
    "div",
    { class: `toast ${kind}` },
    h("span", { class: "toast-message", text: message }),
    iconButton(icon("close"), "Dismiss", dismiss, "icon-button small"),
  );
  container.append(element);
  setTimeout(dismiss, timeout);
}

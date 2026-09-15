// Modal dialogs on the native <dialog> element, which handles focus and
// Escape. Each returns a promise for the user's answer.

import { h } from "./dom";

export interface Choice<T> {
  label: string;
  value: T;
  /** The highlighted default action. */
  primary?: boolean;
  /** A destructive action. */
  danger?: boolean;
}

let dialogs = 0;

function open(content: HTMLElement[], className = ""): HTMLDialogElement {
  const dialog = h("dialog", { class: `dialog ${className}`.trim() }, ...content);
  // Screen readers announce the dialog by its title and message.
  const id = `dialog-${++dialogs}`;
  const title = dialog.querySelector(".dialog-title");
  const message = dialog.querySelector(".dialog-message");
  if (title) {
    title.id = `${id}-title`;
    dialog.setAttribute("aria-labelledby", title.id);
  }
  if (message) {
    message.id = `${id}-message`;
    dialog.setAttribute("aria-describedby", message.id);
  }
  document.body.append(dialog);
  dialog.addEventListener("close", () => {
    // Removed a little later, so the answer's handlers still find it.
    setTimeout(() => dialog.remove(), 200);
  });
  dialog.showModal();
  return dialog;
}

/** Asks a question with buttons; resolves to the chosen value, or `cancel` on Escape. */
export function choose<T>(title: string, message: string, choices: Choice<T>[], cancel: T): Promise<T> {
  return new Promise((resolve) => {
    let answer = cancel;
    const buttons = choices.map((choice) =>
      h("button", {
        class: `button${choice.primary ? " primary" : ""}${choice.danger ? " danger" : ""}`,
        text: choice.label,
        attrs: { type: "button" },
        on: {
          click: () => {
            answer = choice.value;
            dialog.close();
          },
        },
      }),
    );
    const dialog = open([
      h("h2", { class: "dialog-title", text: title }),
      h("p", { class: "dialog-message", text: message }),
      h("div", { class: "dialog-actions" }, ...buttons),
    ]);
    dialog.addEventListener("close", () => resolve(answer));
    const primary = choices.findIndex((choice) => choice.primary);
    buttons[primary < 0 ? 0 : primary]?.focus();
  });
}

export function confirm(title: string, message: string, action: string, danger = false): Promise<boolean> {
  return choose(title, message, [{ label: "Cancel", value: false }, { label: action, value: true, primary: !danger, danger }], false);
}

/**
 * Asks for a line of text. `validate` returns an error to show, or null. The
 * initial selection covers `select` (default: all of `initial`).
 */
export function prompt(
  title: string,
  label: string,
  initial: string,
  validate: (value: string) => string | null,
  select: [number, number] = [0, initial.length],
): Promise<string | null> {
  return new Promise((resolve) => {
    let answer: string | null = null;
    const input = h("input", { class: "text-input", attrs: { type: "text", spellcheck: "false", "aria-label": label } });
    input.value = initial;
    const error = h("p", { class: "dialog-error", attrs: { role: "alert" } });
    const ok = h("button", { class: "button primary", text: "OK", attrs: { type: "submit" } });
    const check = () => {
      const problem = input.value.trim() === "" ? "" : validate(input.value.trim());
      error.textContent = problem ?? "";
      ok.disabled = problem !== null;
      return problem === null;
    };
    const form = h(
      "form",
      {
        attrs: { method: "dialog" },
        on: {
          submit: (event) => {
            event.preventDefault();
            if (!check()) return;
            answer = input.value.trim();
            dialog.close();
          },
        },
      },
      h("h2", { class: "dialog-title", text: title }),
      h("label", { class: "dialog-label", text: label }),
      input,
      error,
      h(
        "div",
        { class: "dialog-actions" },
        h("button", { class: "button", text: "Cancel", attrs: { type: "button" }, on: { click: () => dialog.close() } }),
        ok,
      ),
    );
    input.addEventListener("input", check);
    const dialog = open([form]);
    dialog.addEventListener("close", () => resolve(answer));
    check();
    input.focus();
    input.setSelectionRange(select[0], select[1]);
  });
}

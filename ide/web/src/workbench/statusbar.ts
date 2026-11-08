import type { RunStatus } from "../panel/runner";
import { fileName } from "./documents";
import { h } from "./dom";
import { icon } from "./icons";

export interface StatusBarActions {
  problems(): void;
  output(): void;
}

export class StatusBar {
  readonly element: HTMLElement;
  private readonly problems: HTMLButtonElement;
  private readonly run: HTMLButtonElement;
  private readonly cursor: HTMLElement;
  private readonly language: HTMLElement;

  constructor(actions: StatusBarActions) {
    this.problems = h("button", { class: "status-item", attrs: { type: "button" }, on: { click: () => actions.problems() } });
    this.run = h("button", { class: "status-item status-run", attrs: { type: "button" }, on: { click: () => actions.output() } });
    this.cursor = h("span", { class: "status-item status-cursor" });
    this.language = h("span", { class: "status-item" });
    this.element = h(
      "footer",
      { class: "statusbar", attrs: { "aria-label": "Status" } },
      this.problems,
      this.run,
      h("div", { class: "titlebar-spacer" }),
      this.cursor,
      this.language,
    );
    this.setProblems(0);
    this.setRun({ kind: "idle" });
    this.setCursor(null);
  }

  setProblems(errors: number, warnings = 0): void {
    this.problems.classList.toggle("has-problems", errors > 0);
    this.problems.classList.toggle("has-warnings", errors === 0 && warnings > 0);
    this.problems.title = "Show problems";
    const counted = (count: number, word: string) => (count === 1 ? `1 ${word}` : `${count} ${word}s`);
    const parts = [...(errors > 0 ? [counted(errors, "error")] : []), ...(warnings > 0 ? [counted(warnings, "warning")] : [])];
    this.problems.replaceChildren(
      icon(errors > 0 ? "error" : warnings > 0 ? "warning" : "check"),
      h("span", { text: parts.length === 0 ? "No problems" : parts.join(", ") }),
    );
  }

  setRun(status: RunStatus): void {
    const set = (className: string, text: string, iconName: Parameters<typeof icon>[0] | null) => {
      this.run.className = `status-item status-run ${className}`;
      this.run.hidden = text === "";
      this.run.title = "Show output";
      this.run.replaceChildren(iconName ? icon(iconName) : h("span", { class: "status-spinner", attrs: { "aria-hidden": "true" } }), h("span", { text }));
    };
    switch (status.kind) {
      case "idle":
        set("", "", "play");
        break;
      case "building":
        set("busy", `Building ${fileName(status.path)}…`, null);
        break;
      case "running":
        set("running", `Running ${fileName(status.path)}`, "play");
        break;
      case "exited":
        set(status.ok ? "ok" : "failed", `${fileName(status.path)} ${status.text} · ${status.duration}`, status.ok ? "check" : "error");
        break;
      case "built":
        set("ok", `Built ${fileName(status.path)}`, "check");
        break;
      case "failed":
        set("failed", `${fileName(status.path)}: build failed`, "error");
        break;
    }
  }

  setCursor(cursor: { line: number; col: number; selected: number } | null, language: string | null = null): void {
    this.cursor.hidden = cursor === null;
    this.language.hidden = language === null;
    if (cursor) {
      this.cursor.textContent = `Ln ${cursor.line}, Col ${cursor.col}${cursor.selected > 0 ? ` (${cursor.selected} selected)` : ""}`;
    }
    this.language.textContent = language ?? "";
  }
}

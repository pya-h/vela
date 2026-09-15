// The tab strip: one tab per open document, with a dot for unsaved changes
// that turns into a close button on hover. Middle-click closes. Keyboard:
// arrows move between tabs, Enter opens, Delete closes.

import { type Documents, fileName, isDirty, parentFolder } from "./documents";
import { h } from "./dom";
import { icon } from "./icons";

export interface TabActions {
  activate(path: string): void;
  /** Closes a tab, resolving once it is closed (or the user kept it). */
  close(path: string): Promise<unknown>;
}

export class Tabs {
  readonly element: HTMLElement;
  private rendered = "";

  constructor(
    private readonly documents: Documents,
    private readonly actions: TabActions,
  ) {
    this.element = h("div", { class: "tabs", attrs: { role: "tablist", "aria-label": "Open files" } });
    documents.subscribe(() => this.render());
  }

  private render(): void {
    const docs = this.documents.all;
    // Re-render only when something visible changed (this runs on every edit).
    const key = JSON.stringify(docs.map((d) => [d.path, isDirty(d), d.disk, d.path === this.documents.active]));
    if (key === this.rendered) return;
    const added = new Set(docs.map((d) => d.path));
    for (const tab of this.element.querySelectorAll<HTMLElement>(".tab")) added.delete(tab.dataset["path"] ?? "");
    this.rendered = key;

    const names = new Map<string, number>();
    for (const doc of docs) names.set(fileName(doc.path), (names.get(fileName(doc.path)) ?? 0) + 1);

    this.element.replaceChildren(
      ...docs.map((doc) => {
        const name = fileName(doc.path);
        const active = doc.path === this.documents.active;
        const dirty = isDirty(doc);
        const classes = ["tab"];
        if (active) classes.push("active");
        if (dirty) classes.push("dirty");
        if (doc.disk !== "in-sync") classes.push(doc.disk);
        if (added.has(doc.path)) classes.push("opening");
        const status = doc.disk === "deleted" ? " (deleted)" : doc.disk === "changed" ? " (changed on disk)" : "";
        const tab = h(
          "div",
          {
            class: classes.join(" "),
            title: doc.path + status,
            attrs: { role: "tab", "aria-selected": String(active), tabindex: active ? "0" : "-1", "data-path": doc.path },
            on: {
              click: () => this.actions.activate(doc.path),
              auxclick: (event) => {
                if (event.button === 1) this.actions.close(doc.path);
              },
              keydown: (event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  this.actions.activate(doc.path);
                } else if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
                  // Arrows move between tabs, as in any tab list.
                  event.preventDefault();
                  const index = docs.indexOf(doc) + (event.key === "ArrowRight" ? 1 : -1);
                  this.focusTab(docs[(index + docs.length) % docs.length]?.path);
                } else if (event.key === "Delete") {
                  // Focus stays in the tab list, on the tab that is now active.
                  void this.actions.close(doc.path).then(() => this.focusTab(this.documents.active ?? undefined));
                }
              },
            },
          },
          h("span", { class: "tab-name", text: name }),
          (names.get(name) ?? 0) > 1 ? h("span", { class: "tab-detail", text: parentFolder(doc.path) || "/" }) : null,
          h(
            "button",
            {
              class: "tab-close",
              title: dirty ? "Unsaved changes: close" : "Close",
              attrs: { type: "button", "aria-label": `Close ${name}`, tabindex: "-1" },
              on: {
                click: (event) => {
                  event.stopPropagation();
                  this.actions.close(doc.path);
                },
              },
            },
            h("span", { class: "tab-dot", attrs: { "aria-hidden": "true" } }),
            icon("close", "icon tab-x"),
          ),
        );
        return tab;
      }),
    );
    this.element.querySelector(".tab.active")?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }

  private focusTab(path: string | undefined): void {
    const tabs = [...this.element.querySelectorAll<HTMLElement>(".tab")];
    for (const tab of tabs) tab.tabIndex = tab.dataset["path"] === path ? 0 : -1;
    tabs.find((tab) => tab.dataset["path"] === path)?.focus();
  }
}

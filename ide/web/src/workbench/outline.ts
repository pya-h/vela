// The outline of the open file: tasks and the types they take, shapes and their
// fields, kinds and their variants, and the methods of both. The declaration
// around the cursor is highlighted.

import type { OutlineItem, SymbolKind } from "../analysis/protocol";
import { h } from "./dom";

export const SYMBOL_BADGES: Record<SymbolKind, { text: string; className: string }> = {
  task: { text: "T", className: "kind-task" },
  shape: { text: "S", className: "kind-shape" },
  kind: { text: "K", className: "kind-kind" },
  variant: { text: "V", className: "kind-variant" },
  method: { text: "M", className: "kind-method" },
  field: { text: "F", className: "kind-field" },
  parameter: { text: "P", className: "kind-parameter" },
  variable: { text: "B", className: "kind-variable" },
  region: { text: "R", className: "kind-region" },
  "type-param": { text: "T", className: "kind-type-param" },
  // The outline lists a file's own declarations, never a module it uses.
  module: { text: "M", className: "kind-module" },
};

/** Every item with its depth, parents before children. */
export function flattenOutline(items: OutlineItem[], depth = 0): { item: OutlineItem; depth: number }[] {
  return items.flatMap((item) => [{ item, depth }, ...flattenOutline(item.children, depth + 1)]);
}

export class Outline {
  readonly element: HTMLElement;
  private readonly list: HTMLElement;
  private items: OutlineItem[] | null = null;
  private cursor = 0;
  private rendered = "";

  constructor(private readonly jump: (item: OutlineItem) => void) {
    this.list = h("div", { class: "outline-list" });
    this.element = h(
      "section",
      { class: "sidebar-section outline" },
      h("header", { class: "section-header" }, h("h2", { class: "section-title", text: "Outline" })),
      this.list,
    );
  }

  /** Shows `items` (null when no Vela file is open) with the cursor at `cursor`. */
  update(items: OutlineItem[] | null, cursor: number): void {
    this.items = items;
    this.cursor = cursor;
    this.render();
  }

  private render(): void {
    const rows = this.items ? flattenOutline(this.items) : [];
    // The innermost declaration containing the cursor.
    let current: OutlineItem | null = null;
    for (const { item } of rows) {
      if (item.from <= this.cursor && this.cursor <= item.to) current = item;
    }
    const key = JSON.stringify([this.items === null, rows.map(({ item, depth }) => [item.name, item.kind, item.detail, item.nameFrom, depth]), current?.nameFrom]);
    if (key === this.rendered) return;
    this.rendered = key;

    if (this.items === null) {
      this.list.replaceChildren(h("p", { class: "outline-empty", text: "Open a .vela file to see its outline." }));
      return;
    }
    if (rows.length === 0) {
      this.list.replaceChildren(h("p", { class: "outline-empty", text: "No tasks, shapes, or kinds yet." }));
      return;
    }
    this.list.replaceChildren(
      ...rows.map(({ item, depth }) => {
        const badge = SYMBOL_BADGES[item.kind];
        const row = h(
          "button",
          {
            class: `outline-row${item === current ? " current" : ""}`,
            title: `${item.name}${item.detail ? ` ${item.detail}` : ""}`,
            attrs: { type: "button" },
            on: { click: () => this.jump(item) },
          },
          h("span", { class: `badge ${badge.className}`, text: badge.text }),
          h("span", { class: "outline-name", text: item.name }),
          item.detail ? h("span", { class: "outline-detail", text: item.detail }) : null,
        );
        row.style.setProperty("--depth", String(depth));
        return row;
      }),
    );
  }
}

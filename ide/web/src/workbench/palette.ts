// A quick picker: type to filter a list, arrows to move, Enter to choose.

import { h } from "./dom";
import { type Match, fuzzyFilter } from "./fuzzy";

export interface PickItem {
  label: string;
  /** Shown dimmed after the label, and matched too. */
  detail?: string;
  /** A short badge before the label, such as a symbol kind. */
  badge?: { text: string; className: string };
}

const MAX_SHOWN = 200;

function highlighted(text: string, positions: Set<number>, offset: number): Node[] {
  const nodes: Node[] = [];
  let run = "";
  let runMatched = false;
  const flush = () => {
    if (!run) return;
    nodes.push(runMatched ? h("mark", { text: run }) : document.createTextNode(run));
    run = "";
  };
  // Positions are string indices (UTF-16 code units), as the matcher counts.
  for (let i = 0; i < text.length; i++) {
    const matched = positions.has(i + offset);
    if (matched !== runMatched) {
      flush();
      runMatched = matched;
    }
    run += text[i];
  }
  flush();
  return nodes;
}

let pickers = 0;

/** Shows the picker; resolves to the chosen item, or null if dismissed. */
export function pick<T extends PickItem>(placeholder: string, items: readonly T[], empty = "No matches"): Promise<T | null> {
  return new Promise((resolve) => {
    let chosen: T | null = null;
    let results: { item: T; match: Match }[] = [];
    let selected = 0;

    // Unique ids: a closed picker stays in the page for a moment.
    const id = `palette-${++pickers}`;
    const input = h("input", {
      class: "palette-input",
      attrs: { type: "text", placeholder, spellcheck: "false", role: "combobox", "aria-expanded": "true", "aria-controls": `${id}-list`, "aria-label": placeholder },
    });
    const list = h("ul", { class: "palette-list", attrs: { role: "listbox", id: `${id}-list` } });
    const dialog = h("dialog", { class: "palette", attrs: { "aria-label": placeholder } }, input, list);

    const render = () => {
      const text = (item: T) => (item.detail ? `${item.label} ${item.detail}` : item.label);
      results = fuzzyFilter(items, input.value, text).slice(0, MAX_SHOWN);
      selected = Math.min(selected, Math.max(0, results.length - 1));
      list.replaceChildren(
        ...(results.length === 0
          ? [h("li", { class: "palette-empty", text: empty })]
          : results.map(({ item, match }, index) => {
              const positions = new Set(match.positions);
              const row = h(
                "li",
                {
                  class: `palette-item${index === selected ? " selected" : ""}`,
                  attrs: { role: "option", "aria-selected": String(index === selected), id: `${id}-option-${index}` },
                  on: {
                    mousedown: (event) => event.preventDefault(),
                    click: () => choose(index),
                    mousemove: () => {
                      if (selected !== index) {
                        selected = index;
                        render();
                      }
                    },
                  },
                },
                item.badge ? h("span", { class: `badge ${item.badge.className}`, text: item.badge.text }) : null,
                h("span", { class: "palette-label" }, ...highlighted(item.label, positions, 0)),
                item.detail ? h("span", { class: "palette-detail" }, ...highlighted(item.detail, positions, item.label.length + 1)) : null,
              );
              return row;
            })),
      );
      input.setAttribute("aria-activedescendant", results.length ? `${id}-option-${selected}` : "");
      list.querySelector(".selected")?.scrollIntoView({ block: "nearest" });
    };

    const choose = (index: number) => {
      const result = results[index];
      if (!result) return;
      chosen = result.item;
      dialog.close();
    };

    input.addEventListener("input", () => {
      selected = 0;
      render();
    });
    input.addEventListener("keydown", (event) => {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const count = results.length;
        if (count) selected = (selected + (event.key === "ArrowDown" ? 1 : count - 1)) % count;
        render();
      } else if (event.key === "Enter") {
        event.preventDefault();
        choose(selected);
      }
    });
    dialog.addEventListener("click", (event) => {
      if (event.target === dialog) dialog.close();
    });
    dialog.addEventListener("close", () => {
      setTimeout(() => dialog.remove(), 200);
      resolve(chosen);
    });

    document.body.append(dialog);
    dialog.showModal();
    render();
    input.focus();
  });
}

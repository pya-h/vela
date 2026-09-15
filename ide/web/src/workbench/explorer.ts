// The file tree: folders expand and collapse (remembered per workspace),
// files open on click, and a context menu or keys create, rename, and delete.
// Keyboard: arrows move and expand, Enter opens, F2 renames, Delete deletes.

import type { Entry, Tree } from "../api/types";
import { parentFolder } from "./documents";
import { h, iconButton } from "./dom";
import { icon } from "./icons";
import { type MenuItem, showMenu } from "./menu";
import { isStringArray, load, save } from "./storage";

export interface ExplorerActions {
  open(path: string): void;
  create(kind: "file" | "folder", folder: string): void;
  rename(entry: Entry): void;
  remove(entry: Entry): void;
  refresh(): void;
}

interface Row {
  entry: Entry;
  depth: number;
}

export class Explorer {
  readonly element: HTMLElement;
  private readonly list: HTMLElement;
  private tree: Tree = { entries: [], truncated: false };
  private rows: Row[] = [];
  private readonly expanded: Set<string>;
  private selected: string | null = null;
  private active: string | null = null;

  constructor(
    private readonly storageKey: string,
    private readonly actions: ExplorerActions,
  ) {
    this.expanded = new Set(load(storageKey, [], isStringArray));
    this.list = h("div", {
      class: "tree",
      attrs: { role: "tree", "aria-label": "Files", tabindex: "-1" },
      on: { keydown: (event) => this.onKey(event), contextmenu: (event) => this.onContextMenu(event, null) },
    });
    this.element = h(
      "section",
      { class: "sidebar-section explorer" },
      h(
        "header",
        { class: "section-header" },
        h("h2", { class: "section-title", text: "Explorer" }),
        h(
          "div",
          { class: "section-actions" },
          iconButton(icon("newFile"), "New file", () => this.actions.create("file", this.selectedFolder())),
          iconButton(icon("newFolder"), "New folder", () => this.actions.create("folder", this.selectedFolder())),
          iconButton(icon("refresh"), "Refresh", () => this.actions.refresh()),
          iconButton(icon("collapse"), "Collapse folders", () => this.collapseAll()),
        ),
      ),
      this.list,
    );
  }

  setTree(tree: Tree): void {
    this.tree = tree;
    this.render();
  }

  /** Highlights the open file and expands the folders around it. */
  setActive(path: string | null): void {
    this.active = path;
    if (path !== null) {
      for (let folder = parentFolder(path); folder !== ""; folder = parentFolder(folder)) {
        this.expanded.add(folder);
      }
      this.selected = path;
    }
    this.render();
    this.list.querySelector(".tree-row.active")?.scrollIntoView({ block: "nearest" });
  }

  /** Expands a folder, such as one just created or containing a new file. */
  expand(folder: string): void {
    for (let f = folder; f !== ""; f = parentFolder(f)) this.expanded.add(f);
    this.persist();
  }

  /** The folder new entries go in: the selected folder, or the selected file's folder. */
  selectedFolder(): string {
    const row = this.rows.find((r) => r.entry.path === this.selected);
    if (!row) return "";
    return row.entry.kind === "folder" ? row.entry.path : parentFolder(row.entry.path);
  }

  /** Follows a rename in the remembered expanded folders. */
  renamed(from: string, to: string): void {
    for (const path of [...this.expanded]) {
      if (path === from || path.startsWith(`${from}/`)) {
        this.expanded.delete(path);
        this.expanded.add(to + path.slice(from.length));
      }
    }
    if (this.selected !== null && (this.selected === from || this.selected.startsWith(`${from}/`))) {
      this.selected = to + this.selected.slice(from.length);
    }
    this.persist();
  }

  private collapseAll(): void {
    this.expanded.clear();
    this.persist();
    this.render();
  }

  private persist(): void {
    save(this.storageKey, [...this.expanded]);
  }

  private render(): void {
    this.rows = [];
    const walk = (entries: Entry[], depth: number) => {
      for (const entry of entries) {
        this.rows.push({ entry, depth });
        if (entry.kind === "folder" && this.expanded.has(entry.path)) walk(entry.children ?? [], depth + 1);
      }
    };
    walk(this.tree.entries, 0);
    if (this.selected !== null && !this.rows.some((r) => r.entry.path === this.selected)) this.selected = null;
    const focusable = this.selected ?? this.rows[0]?.entry.path ?? null;
    const hadFocus = this.list.contains(document.activeElement);

    const elements: HTMLElement[] = this.rows.map(({ entry, depth }) => {
      const isFolder = entry.kind === "folder";
      const open = isFolder && this.expanded.has(entry.path);
      const classes = ["tree-row", isFolder ? "folder" : "file"];
      if (entry.path === this.active) classes.push("active");
      if (entry.path === this.selected) classes.push("selected");
      if (open) classes.push("open");
      if (!isFolder && entry.name.endsWith(".vela")) classes.push("vela");
      const row = h(
        "div",
        {
          class: classes.join(" "),
          title: entry.path,
          attrs: {
            role: "treeitem",
            "aria-level": String(depth + 1),
            "aria-selected": String(entry.path === this.selected),
            tabindex: entry.path === focusable ? "0" : "-1",
            "data-path": entry.path,
            ...(isFolder ? { "aria-expanded": String(open) } : {}),
          },
          on: {
            click: () => this.activate(entry),
            contextmenu: (event) => this.onContextMenu(event, entry),
          },
        },
        isFolder ? icon("chevron", "icon chevron") : h("span", { class: "chevron-space" }),
        icon(isFolder ? "folder" : "file", "icon entry-icon"),
        h("span", { class: "tree-name", text: entry.name }),
      );
      // The indentation, through the CSSOM (allowed by the page's policy).
      row.style.setProperty("--depth", String(depth));
      return row;
    });
    if (this.tree.entries.length === 0) {
      elements.push(h("p", { class: "tree-empty", text: "This folder is empty." }));
    }
    if (this.tree.truncated) {
      elements.push(h("p", { class: "tree-empty", text: "Some files aren't shown: the folder is very large." }));
    }
    this.list.replaceChildren(...elements);
    if (hadFocus) this.focusRow(this.selected);
  }

  private activate(entry: Entry): void {
    this.selected = entry.path;
    if (entry.kind === "folder") {
      if (this.expanded.has(entry.path)) this.expanded.delete(entry.path);
      else this.expanded.add(entry.path);
      this.persist();
      this.render();
      this.focusRow(entry.path);
    } else {
      this.render();
      this.actions.open(entry.path);
    }
  }

  /** Focuses a row: the active file's, or else the first. */
  focus(): void {
    const rows = [...this.list.querySelectorAll<HTMLElement>(".tree-row")];
    (rows.find((row) => row.tabIndex === 0) ?? rows[0])?.focus();
  }

  private focusRow(path: string | null): void {
    if (path === null) return;
    const row = [...this.list.querySelectorAll<HTMLElement>(".tree-row")].find((r) => r.dataset["path"] === path);
    row?.focus();
  }

  private onKey(event: KeyboardEvent): void {
    const index = this.rows.findIndex((r) => r.entry.path === this.selected);
    const row = this.rows[index];
    const select = (i: number) => {
      const target = this.rows[Math.max(0, Math.min(this.rows.length - 1, i))];
      if (!target) return;
      this.selected = target.entry.path;
      this.render();
      this.focusRow(target.entry.path);
    };
    switch (event.key) {
      case "ArrowDown":
        select(index + 1);
        break;
      case "ArrowUp":
        select(index - 1);
        break;
      case "Home":
        select(0);
        break;
      case "End":
        select(this.rows.length - 1);
        break;
      case "ArrowRight":
        if (row?.entry.kind === "folder" && !this.expanded.has(row.entry.path)) this.activate(row.entry);
        else select(index + 1);
        break;
      case "ArrowLeft":
        if (row?.entry.kind === "folder" && this.expanded.has(row.entry.path)) this.activate(row.entry);
        else if (row && parentFolder(row.entry.path) !== "") select(this.rows.findIndex((r) => r.entry.path === parentFolder(row.entry.path)));
        break;
      case "Enter":
      case " ":
        if (row) this.activate(row.entry);
        break;
      case "F2":
        if (row) this.actions.rename(row.entry);
        break;
      case "Delete":
        if (row) this.actions.remove(row.entry);
        break;
      default:
        return;
    }
    event.preventDefault();
  }

  private onContextMenu(event: MouseEvent, entry: Entry | null): void {
    event.preventDefault();
    event.stopPropagation();
    if (entry) {
      this.selected = entry.path;
      this.render();
    }
    const folder = entry === null ? "" : entry.kind === "folder" ? entry.path : parentFolder(entry.path);
    const items: (MenuItem | "separator")[] = [
      { label: "New File…", run: () => this.actions.create("file", folder) },
      { label: "New Folder…", run: () => this.actions.create("folder", folder) },
    ];
    if (entry) {
      items.push(
        "separator",
        { label: "Rename…", shortcut: "F2", run: () => this.actions.rename(entry) },
        { label: "Delete", shortcut: "Del", danger: true, run: () => this.actions.remove(entry) },
      );
    }
    showMenu(event.clientX, event.clientY, items);
  }
}

/** Every file in the tree, for the quick picker. */
export function allFiles(entries: Entry[]): Entry[] {
  return entries.flatMap((entry) => (entry.kind === "folder" ? allFiles(entry.children ?? []) : [entry]));
}

/** Whether the tree has an entry at `path`. */
export function hasEntry(entries: Entry[], path: string): boolean {
  return entries.some((entry) => entry.path === path || (entry.kind === "folder" && path.startsWith(`${entry.path}/`) && hasEntry(entry.children ?? [], path)));
}

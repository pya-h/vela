// The IDE window: title bar, sidebar (explorer and outline), tabs, and the
// editor, and the file operations that connect them to the server.

import { isolateHistory } from "@codemirror/commands";
import { EditorState, type Text } from "@codemirror/state";
import { EditorView, type ViewUpdate } from "@codemirror/view";

import type { OutlineItem } from "../analysis/protocol";
import { ApiError, type StudioApi } from "../api/client";
import type { Entry, Info, Tree } from "../api/types";
import { editorExtensions } from "../editor/setup";
import type { Analyzer } from "../editor/vela";
import { choose, confirm, prompt } from "./dialogs";
import { Documents, type OpenDocument, fileName, isDirty, nameProblem, parentFolder } from "./documents";
import { h, iconButton, shortcutLabel } from "./dom";
import { Explorer, allFiles, hasEntry } from "./explorer";
import { icon } from "./icons";
import { Outline, SYMBOL_BADGES, flattenOutline } from "./outline";
import { pick } from "./palette";
import { isBoolean, isNumber, isStringArray, load, save } from "./storage";
import { Tabs } from "./tabs";
import { ThemeSwitch } from "./theme";
import { toast } from "./toast";

const SIDEBAR = { min: 180, max: 560, initial: 260 };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export class Workbench {
  readonly element: HTMLElement;
  private readonly documents = new Documents();
  private readonly view: EditorView;
  private readonly editorHost: HTMLElement;
  private readonly welcome: HTMLElement;
  private readonly explorer: Explorer;
  private readonly outline: Outline;
  private tree: Tree;
  private sidebarWidth = load("sidebar-width", SIDEBAR.initial, isNumber);
  private sidebarVisible = load("sidebar-visible", true, isBoolean);
  private outlineTimer: ReturnType<typeof setTimeout> | undefined;
  private lastDiskCheck = 0;
  /** Guards against opening the same file twice while it loads. */
  private readonly opening = new Map<string, Promise<void>>();

  constructor(
    private readonly api: StudioApi,
    private readonly info: Info,
    private readonly analyzer: Analyzer,
    private readonly nonce: string,
    tree: Tree,
  ) {
    this.tree = tree;
    const workspaceKey = info.workspace.root;
    this.explorer = new Explorer(`expanded:${workspaceKey}`, {
      open: (path) => void this.openFile(path),
      create: (kind, folder) => void this.createEntry(kind, folder),
      rename: (entry) => void this.renameEntry(entry),
      remove: (entry) => void this.deleteEntry(entry),
      refresh: () => void this.refreshTree(),
    });
    this.explorer.setTree(tree);
    this.outline = new Outline((item) => this.jumpTo(item));
    const tabs = new Tabs(this.documents, { activate: (path) => this.activate(path), close: (path) => void this.close(path) });

    this.view = new EditorView({ state: EditorState.create({ extensions: EditorView.cspNonce.of(nonce) }) });
    this.editorHost = h("div", { class: "editor-host" }, this.view.dom);
    this.welcome = this.buildWelcome();

    const theme = new ThemeSwitch();
    const resizer = h("div", {
      class: "resizer",
      title: "Drag to resize; double-click to reset",
      attrs: { role: "separator", "aria-orientation": "vertical", "aria-label": "Resize sidebar", tabindex: "0" },
    });
    this.element = h(
      "div",
      { class: "app" },
      h(
        "header",
        { class: "titlebar" },
        iconButton(icon("sidebar"), `Toggle sidebar (${shortcutLabel("Mod-b")})`, () => this.toggleSidebar()),
        h("div", { class: "brand" }, h("span", { class: "brand-mark", attrs: { "aria-hidden": "true" }, text: "◆" }), h("span", { text: "Vela Studio" })),
        h("span", { class: "titlebar-workspace", title: info.workspace.root, text: info.workspace.name }),
        h("div", { class: "titlebar-spacer" }),
        info.cc.available
          ? null
          : h(
              "span",
              {
                class: "chip warning",
                title: `The C compiler '${info.cc.command}' could not be started. Install a C compiler, or set $CC before starting vela-studio.`,
              },
              icon("warning"),
              h("span", { text: "No C compiler" }),
            ),
        iconButton(icon("search"), `Open file (${shortcutLabel("Mod-p")})`, () => void this.quickOpen()),
        theme.button,
      ),
      h(
        "div",
        { class: "workspace" },
        h("aside", { class: "sidebar", attrs: { "aria-label": "Sidebar" } }, this.explorer.element, this.outline.element),
        resizer,
        h("main", { class: "editor-area" }, tabs.element, this.editorHost, this.welcome),
      ),
    );
    this.setupResizer(resizer);
    this.applySidebar();
    this.documents.subscribe(() => this.showEditorOrWelcome());
    this.showEditorOrWelcome();
    this.outline.update(null, 0);

    window.addEventListener("keydown", (event) => this.onGlobalKey(event));
    window.addEventListener("focus", () => void this.checkDisk());
    document.addEventListener("visibilitychange", () => {
      if (document.visibilityState === "visible") void this.checkDisk();
    });
    window.addEventListener("beforeunload", (event) => {
      if (this.documents.all.some(isDirty)) event.preventDefault();
    });
    void this.restoreTabs();
  }

  // Documents and the editor ------------------------------------------------------

  private createState(path: string, content: string): EditorState {
    return EditorState.create({
      doc: content,
      extensions: editorExtensions({
        nonce: this.nonce,
        analyzer: path.endsWith(".vela") ? this.analyzer : null,
        onUpdate: EditorView.updateListener.of((update) => this.onEditorUpdate(update)),
        keys: [],
      }),
    });
  }

  private onEditorUpdate(update: ViewUpdate): void {
    const doc = this.documents.current;
    if (!doc || update.view !== this.view) return;
    doc.state = update.state;
    if (update.docChanged) this.documents.changed();
    if (update.docChanged || update.selectionSet) this.scheduleOutline();
  }

  async openFile(path: string): Promise<void> {
    if (this.documents.get(path)) {
      this.activate(path);
      return;
    }
    let loading = this.opening.get(path);
    if (!loading) {
      loading = (async () => {
        try {
          const file = await this.api.readFile(path);
          if (this.documents.get(path)) return;
          const state = this.createState(path, file.content);
          this.documents.add({ path, state, saved: state.doc, hash: file.hash, disk: "in-sync", scrollTop: 0 });
        } catch (error) {
          toast(`Can't open ${path}: ${message(error)}`, "error");
        } finally {
          this.opening.delete(path);
        }
      })();
      this.opening.set(path, loading);
    }
    await loading;
    if (this.documents.get(path)) this.activate(path);
  }

  activate(path: string): void {
    const doc = this.documents.get(path);
    if (!doc) return;
    const previous = this.documents.current;
    if (previous && previous !== doc) {
      previous.state = this.view.state;
      previous.scrollTop = this.view.scrollDOM.scrollTop;
    }
    if (previous !== doc || this.view.state !== doc.state) {
      this.documents.active = path;
      this.view.setState(doc.state);
      const scrollTop = doc.scrollTop;
      requestAnimationFrame(() => {
        this.view.scrollDOM.scrollTop = scrollTop;
      });
    }
    this.documents.changed();
    this.explorer.setActive(path);
    this.updateOutline();
    this.persistTabs();
    this.view.focus();
  }

  async close(path: string): Promise<boolean> {
    const doc = this.documents.get(path);
    if (!doc) return true;
    if (isDirty(doc)) {
      const answer = await choose(
        "Save changes?",
        `${fileName(path)} has unsaved changes. They are lost if you don't save them.`,
        [
          { label: "Cancel", value: "cancel" },
          { label: "Don't Save", value: "discard", danger: true },
          { label: "Save", value: "save", primary: true },
        ],
        "cancel",
      );
      if (answer === "cancel") return false;
      if (answer === "save" && !(await this.save(doc))) return false;
    }
    this.discard(path);
    return true;
  }

  /** Closes a tab without asking. */
  private discard(path: string): void {
    const wasActive = this.documents.active === path;
    if (wasActive) this.documents.current!.state = this.view.state;
    const next = this.documents.remove(path);
    if (wasActive && next !== null) this.activate(next);
    else if (next === null) {
      this.explorer.setActive(null);
      this.updateOutline();
    }
    this.persistTabs();
  }

  private showEditorOrWelcome(): void {
    const empty = this.documents.all.length === 0;
    this.editorHost.hidden = empty;
    this.welcome.hidden = !empty;
  }

  /** The latest state of a document (the editor's, if it is the open one). */
  private latest(doc: OpenDocument): EditorState {
    return doc === this.documents.current ? this.view.state : doc.state;
  }

  // Saving and changes on disk ------------------------------------------------------

  async saveActive(): Promise<void> {
    const doc = this.documents.current;
    if (doc) await this.save(doc);
  }

  async saveAll(): Promise<void> {
    for (const doc of this.documents.all.filter(isDirty)) {
      if (!(await this.save(doc))) return;
    }
  }

  /** Saves a document; resolves to whether it was saved. */
  async save(doc: OpenDocument, overwrite = false): Promise<boolean> {
    const text = this.latest(doc).doc;
    try {
      if (doc.disk === "deleted") {
        await this.api.create(doc.path, "file").catch((error: unknown) => {
          if (!(error instanceof ApiError && error.conflict)) throw error;
        });
        await this.refreshTree();
      }
      const base = overwrite || doc.disk === "deleted" ? undefined : doc.hash;
      const { hash } = await this.api.writeFile(doc.path, text.toString(), base);
      this.documents.markSaved(doc.path, text, hash);
      this.startUndoStep(doc);
      return true;
    } catch (error) {
      if (error instanceof ApiError && error.conflict && typeof error.body["hash"] === "string") {
        return this.resolveConflict(doc);
      }
      if (error instanceof ApiError && error.notFound) {
        doc.disk = "deleted";
        this.documents.changed();
        const recreate = await confirm("File deleted", `${doc.path} no longer exists on disk. Save it to create it again?`, "Save");
        return recreate ? this.save(doc) : false;
      }
      toast(`Couldn't save ${doc.path}: ${message(error)}`, "error");
      return false;
    }
  }

  private async resolveConflict(doc: OpenDocument): Promise<boolean> {
    const answer = await choose(
      "File changed on disk",
      `${doc.path} was changed by another program after you opened it. Overwrite it with your version, or reload it and lose your changes?`,
      [
        { label: "Cancel", value: "cancel" },
        { label: "Reload", value: "reload", danger: true },
        { label: "Overwrite", value: "overwrite", primary: true },
      ],
      "cancel",
    );
    if (answer === "overwrite") return this.save(doc, true);
    if (answer === "reload") await this.reload(doc);
    return false;
  }

  /** Replaces a document's text with the file on disk (undoable). */
  private async reload(doc: OpenDocument): Promise<void> {
    try {
      const file = await this.api.readFile(doc.path);
      this.replaceText(doc, file.content, file.hash);
    } catch (error) {
      toast(`Couldn't reload ${doc.path}: ${message(error)}`, "error");
    }
  }

  /** Keeps edits after a save out of the undo step of the edits before it. */
  private startUndoStep(doc: OpenDocument): void {
    const spec = { annotations: isolateHistory.of("after") };
    if (doc === this.documents.current) {
      this.view.dispatch(spec);
    } else {
      doc.state = doc.state.update(spec).state;
    }
  }

  private replaceText(doc: OpenDocument, content: string, hash: string): void {
    const current = this.latest(doc);
    const spec = { changes: { from: 0, to: current.doc.length, insert: content } };
    let text: Text;
    if (doc === this.documents.current) {
      this.view.dispatch(spec);
      doc.state = this.view.state;
      text = this.view.state.doc;
    } else {
      doc.state = current.update(spec).state;
      text = doc.state.doc;
    }
    this.documents.markSaved(doc.path, text, hash);
  }

  /** Notices files changed or deleted by other programs (on returning to the page). */
  private async checkDisk(): Promise<void> {
    const now = Date.now();
    if (now - this.lastDiskCheck < 1000) return;
    this.lastDiskCheck = now;
    void this.refreshTree();
    for (const doc of this.documents.all) {
      try {
        const file = await this.api.readFile(doc.path);
        if (file.hash === doc.hash) {
          if (doc.disk !== "in-sync") {
            doc.disk = "in-sync";
            this.documents.changed();
          }
        } else if (!isDirty(doc)) {
          this.replaceText(doc, file.content, file.hash);
          toast(`${fileName(doc.path)} changed on disk and was reloaded.`);
        } else if (doc.disk !== "changed") {
          doc.disk = "changed";
          this.documents.changed();
          toast(`${fileName(doc.path)} changed on disk. Saving will ask before overwriting it.`);
        }
      } catch (error) {
        if (error instanceof ApiError && error.notFound && doc.disk !== "deleted") {
          doc.disk = "deleted";
          this.documents.changed();
        }
      }
    }
  }

  // Files and folders ---------------------------------------------------------------

  async refreshTree(): Promise<void> {
    try {
      this.tree = await this.api.files();
      this.explorer.setTree(this.tree);
    } catch (error) {
      toast(`Couldn't list the files: ${message(error)}`, "error");
    }
  }

  private async createEntry(kind: "file" | "folder", folder: string): Promise<void> {
    const where = folder || this.info.workspace.name;
    const name = await prompt(
      kind === "file" ? "New File" : "New Folder",
      `Name, in ${where}`,
      kind === "file" ? ".vela" : "",
      (value) => nameProblem(value) ?? (hasEntry(this.tree.entries, folder ? `${folder}/${value}` : value) ? "That name is taken." : null),
      [0, 0],
    );
    if (name === null) return;
    const path = folder ? `${folder}/${name}` : name;
    try {
      // Create any folders in the name first.
      const parts = path.split("/");
      for (let i = folder ? folder.split("/").length + 1 : 1; i < parts.length; i++) {
        const parent = parts.slice(0, i).join("/");
        if (!hasEntry(this.tree.entries, parent)) await this.api.create(parent, "folder");
      }
      await this.api.create(path, kind);
      this.explorer.expand(kind === "folder" ? path : parentFolder(path));
      await this.refreshTree();
      if (kind === "file") await this.openFile(path);
    } catch (error) {
      toast(`Couldn't create ${path}: ${message(error)}`, "error");
    }
  }

  private async renameEntry(entry: Entry): Promise<void> {
    const dot = entry.name.lastIndexOf(".");
    const folder = parentFolder(entry.path);
    const name = await prompt(
      entry.kind === "file" ? "Rename File" : "Rename Folder",
      "New name",
      entry.name,
      (value) => {
        if (value === entry.name) return null;
        return nameProblem(value) ?? (hasEntry(this.tree.entries, folder ? `${folder}/${value}` : value) ? "That name is taken." : null);
      },
      [0, entry.kind === "file" && dot > 0 ? dot : entry.name.length],
    );
    if (name === null || name === entry.name) return;
    const to = folder ? `${folder}/${name}` : name;
    try {
      await this.api.rename(entry.path, to);
      const active = this.documents.current;
      if (active) active.state = this.view.state;
      this.documents.renamed(entry.path, to);
      // A file that became (or stopped being) Vela gets the right editor features.
      for (const doc of this.documents.within(to)) {
        const wasVela = (entry.path + doc.path.slice(to.length)).endsWith(".vela");
        if (wasVela !== doc.path.endsWith(".vela")) {
          const saved = doc.saved;
          doc.state = this.createState(doc.path, doc.state.doc.toString());
          doc.saved = saved.eq(doc.state.doc) ? doc.state.doc : saved;
          if (doc === this.documents.current) this.view.setState(doc.state);
        }
      }
      this.explorer.renamed(entry.path, to);
      await this.refreshTree();
      if (this.documents.active) this.explorer.setActive(this.documents.active);
      this.persistTabs();
    } catch (error) {
      toast(`Couldn't rename ${entry.path}: ${message(error)}`, "error");
    }
  }

  private async deleteEntry(entry: Entry): Promise<void> {
    const unsaved = this.documents.within(entry.path).filter(isDirty).length;
    const what = entry.kind === "folder" ? `the folder ${entry.path} and everything in it` : entry.path;
    const warning = unsaved > 0 ? ` ${unsaved === 1 ? "An open file has" : `${unsaved} open files have`} unsaved changes.` : "";
    if (!(await confirm(`Delete ${entry.name}?`, `This permanently deletes ${what}.${warning}`, "Delete", true))) return;
    try {
      await this.api.remove(entry.path);
      for (const doc of this.documents.within(entry.path)) this.discard(doc.path);
      await this.refreshTree();
    } catch (error) {
      toast(`Couldn't delete ${entry.path}: ${message(error)}`, "error");
    }
  }

  // Pickers, outline, and navigation ----------------------------------------------------

  async quickOpen(): Promise<void> {
    const files = allFiles(this.tree.entries).map((entry) => ({ label: entry.name, detail: parentFolder(entry.path), path: entry.path }));
    const chosen = await pick("Open a file by name", files, "No files match");
    if (chosen) await this.openFile(chosen.path);
    else this.view.focus();
  }

  async goToSymbol(): Promise<void> {
    const doc = this.documents.current;
    if (!doc || !doc.path.endsWith(".vela")) {
      toast("Open a .vela file to go to its declarations.");
      return;
    }
    const symbols = flattenOutline(this.analyzer.file(this.view.state.doc).outline).map(({ item }) => ({
      label: item.name,
      detail: item.detail,
      badge: SYMBOL_BADGES[item.kind],
      item,
    }));
    const chosen = await pick("Go to a task, shape, kind, field, or variant", symbols, "No declarations match");
    if (chosen) this.jumpTo(chosen.item);
    else this.view.focus();
  }

  private jumpTo(item: OutlineItem): void {
    this.view.dispatch({
      selection: { anchor: item.nameFrom, head: item.nameTo },
      effects: EditorView.scrollIntoView(item.nameFrom, { y: "center" }),
      userEvent: "select.outline",
    });
    this.view.focus();
  }

  private scheduleOutline(): void {
    clearTimeout(this.outlineTimer);
    this.outlineTimer = setTimeout(() => this.updateOutline(), 150);
  }

  private updateOutline(): void {
    const doc = this.documents.current;
    if (!doc || !doc.path.endsWith(".vela")) {
      this.outline.update(null, 0);
      return;
    }
    this.outline.update(this.analyzer.file(this.view.state.doc).outline, this.view.state.selection.main.head);
  }

  private onGlobalKey(event: KeyboardEvent): void {
    if (document.querySelector("dialog[open]")) return;
    const mod = event.ctrlKey || event.metaKey;
    if (!mod || event.altKey) return;
    const key = event.key.toLowerCase();
    const run = (action: () => void) => {
      event.preventDefault();
      action();
    };
    if (key === "s" && event.shiftKey) run(() => void this.saveAll());
    else if (key === "s") run(() => void this.saveActive());
    else if (key === "p" && !event.shiftKey) run(() => void this.quickOpen());
    else if (key === "o" && event.shiftKey) run(() => void this.goToSymbol());
    else if (key === "b" && !event.shiftKey) run(() => this.toggleSidebar());
  }

  // Layout ------------------------------------------------------------------------

  private toggleSidebar(): void {
    this.sidebarVisible = !this.sidebarVisible;
    save("sidebar-visible", this.sidebarVisible);
    this.applySidebar();
  }

  private applySidebar(): void {
    this.element.classList.toggle("sidebar-hidden", !this.sidebarVisible);
    this.element.style.setProperty("--sidebar-width", `${this.sidebarWidth}px`);
  }

  private setupResizer(resizer: HTMLElement): void {
    const setWidth = (width: number) => {
      this.sidebarWidth = Math.round(Math.max(SIDEBAR.min, Math.min(SIDEBAR.max, width)));
      this.applySidebar();
      save("sidebar-width", this.sidebarWidth);
    };
    resizer.addEventListener("pointerdown", (event) => {
      if (!this.sidebarVisible) return;
      resizer.setPointerCapture(event.pointerId);
      this.element.classList.add("resizing");
      const startX = event.clientX;
      const startWidth = this.sidebarWidth;
      const move = (e: PointerEvent) => setWidth(startWidth + e.clientX - startX);
      const end = () => {
        this.element.classList.remove("resizing");
        resizer.removeEventListener("pointermove", move);
        resizer.removeEventListener("pointerup", end);
        resizer.removeEventListener("pointercancel", end);
      };
      resizer.addEventListener("pointermove", move);
      resizer.addEventListener("pointerup", end);
      resizer.addEventListener("pointercancel", end);
    });
    resizer.addEventListener("dblclick", () => setWidth(SIDEBAR.initial));
    resizer.addEventListener("keydown", (event) => {
      if (event.key === "ArrowLeft") setWidth(this.sidebarWidth - 16);
      else if (event.key === "ArrowRight") setWidth(this.sidebarWidth + 16);
    });
  }

  private buildWelcome(): HTMLElement {
    const examples = allFiles(this.tree.entries)
      .filter((entry) => entry.name.endsWith(".vela"))
      .slice(0, 6);
    const shortcuts: [string, string][] = [
      ["Mod-p", "Open a file"],
      ["Mod-s", "Save"],
      ["Mod-Space", "Complete"],
      ["F12", "Go to definition"],
      ["Mod-Shift-o", "Go to a declaration"],
      ["Mod-/", "Toggle comment"],
      ["Mod-f", "Find and replace"],
      ["Mod-b", "Toggle the sidebar"],
    ];
    return h(
      "section",
      { class: "welcome", attrs: { "aria-label": "Welcome" } },
      h("div", { class: "welcome-mark", attrs: { "aria-hidden": "true" }, text: "◆" }),
      h("h1", { class: "welcome-title", text: "Vela Studio" }),
      h("p", { class: "welcome-subtitle", text: this.info.workspace.root }),
      h(
        "div",
        { class: "welcome-actions" },
        h("button", { class: "button primary", text: "New File…", attrs: { type: "button" }, on: { click: () => void this.createEntry("file", "") } }),
        h("button", { class: "button", text: "Open File…", attrs: { type: "button" }, on: { click: () => void this.quickOpen() } }),
      ),
      examples.length
        ? h(
            "div",
            { class: "welcome-files" },
            h("h2", { text: "Open a program" }),
            ...examples.map((entry) =>
              h(
                "button",
                { class: "welcome-file", attrs: { type: "button" }, on: { click: () => void this.openFile(entry.path) } },
                icon("file"),
                h("span", { text: entry.path }),
              ),
            ),
          )
        : null,
      h(
        "dl",
        { class: "welcome-shortcuts" },
        ...shortcuts.flatMap(([key, label]) => [h("dt", {}, h("kbd", { text: shortcutLabel(key) })), h("dd", { text: label })]),
      ),
    );
  }

  // Remembered tabs -------------------------------------------------------------------

  private persistTabs(): void {
    save(`tabs:${this.info.workspace.root}`, { open: this.documents.all.map((d) => d.path), active: this.documents.active });
  }

  private async restoreTabs(): Promise<void> {
    const remembered = load<{ open: string[]; active: string | null }>(
      `tabs:${this.info.workspace.root}`,
      { open: [], active: null },
      (value): value is { open: string[]; active: string | null } =>
        typeof value === "object" && value !== null && isStringArray((value as { open?: unknown }).open),
    );
    const paths = remembered.open.filter((path) => hasEntry(this.tree.entries, path));
    for (const path of paths) {
      try {
        const file = await this.api.readFile(path);
        if (this.documents.get(path)) continue;
        const state = this.createState(path, file.content);
        this.documents.add({ path, state, saved: state.doc, hash: file.hash, disk: "in-sync", scrollTop: 0 });
      } catch {
        // Skip files that can't be opened any more.
      }
    }
    const active = remembered.active !== null && this.documents.get(remembered.active) ? remembered.active : (this.documents.all[0]?.path ?? null);
    if (active !== null && this.documents.active === null) this.activate(active);
  }
}

// The IDE window: title bar, sidebar (explorer and outline), tabs, the editor,
// the panel (problems, output, terminal), and the status bar, and the file and
// run operations that connect them to the server.

import { isolateHistory } from "@codemirror/commands";
import { EditorState, type Text } from "@codemirror/state";
import { EditorView, type ViewUpdate } from "@codemirror/view";

import type { OutlineItem } from "../analysis/protocol";
import { ApiError, type StudioApi } from "../api/client";
import type { Entry, Info, Tree } from "../api/types";
import { editorExtensions } from "../editor/setup";
import type { Analyzer } from "../editor/vela";
import { Panel } from "../panel/panel";
import { type Problem, collectProblems, problemCount } from "../panel/problems";
import { ProblemsView } from "../panel/problems-view";
import { type RunStatus, Runner } from "../panel/runner";
import { Shell } from "../panel/shell";
import { choose, confirm, prompt } from "./dialogs";
import { Documents, type OpenDocument, fileName, isDirty, isWithin, nameProblem, parentFolder, textChange } from "./documents";
import { h, iconButton, shortcutLabel } from "./dom";
import { Explorer, allFiles, hasEntry } from "./explorer";
import { icon } from "./icons";
import { Outline, SYMBOL_BADGES, flattenOutline } from "./outline";
import { pick } from "./palette";
import { StatusBar } from "./statusbar";
import { isBoolean, isNumber, isStringArray, load, save } from "./storage";
import { Tabs } from "./tabs";
import { ThemeSwitch } from "./theme";
import { toast } from "./toast";

const SIDEBAR = { min: 180, max: 560, initial: 260 };

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

const nextFrame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

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
  /** Saves in progress: a document's next save waits for the one before. */
  private readonly saving = new Map<OpenDocument, Promise<boolean>>();
  private readonly panel: Panel;
  private readonly runner: Runner;
  private readonly shell: Shell;
  private readonly problems: ProblemsView;
  private readonly statusBar: StatusBar;
  private readonly runButton: HTMLButtonElement;
  private readonly buildButton: HTMLButtonElement;
  private problemsTimer: ReturnType<typeof setTimeout> | undefined;

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

    this.runner = new Runner(api, { status: (status) => this.onRunStatus(status), failures: () => this.scheduleProblems() });
    this.shell = new Shell(api);
    this.problems = new ProblemsView((problem) => void this.openProblem(problem));
    const small = (name: Parameters<typeof icon>[0], label: string, action: () => void) => iconButton(icon(name), label, action, "icon-button small");
    this.panel = new Panel({
      problems: {
        element: this.problems.element,
        actions: [],
        shown: (focus) => {
          if (focus) this.problems.element.querySelector<HTMLElement>(".problem")?.focus();
        },
      },
      output: {
        element: this.runner.output.element,
        actions: [small("clear", "Clear the output", () => this.runner.clear())],
        shown: (focus) =>
          void this.runner.output.open().then(() => {
            this.runner.output.fit();
            if (focus) this.runner.output.focus();
          }),
      },
      terminal: {
        element: this.shell.view.element,
        actions: [
          small("refresh", "Restart the terminal", () => void this.shell.restart()),
          small("stop", "Stop the terminal's shell", () => this.shell.kill()),
          small("clear", "Clear the terminal", () => this.shell.clear()),
        ],
        shown: (focus) =>
          void this.shell.view.open().then(() => {
            this.shell.view.fit();
            // Focused before the shell starts: what's typed meanwhile is sent once it has.
            if (focus) this.shell.view.focus();
            return this.shell.show();
          }),
      },
    });
    this.statusBar = new StatusBar({ problems: () => this.panel.show("problems"), output: () => this.panel.show("output") });
    this.runButton = h(
      "button",
      {
        class: "run-button",
        attrs: { type: "button", "data-state": "idle" },
        on: { click: () => (this.runner.status.kind === "running" ? this.runner.stop() : void this.runActive()) },
      },
      h("span", { class: "run-icons", attrs: { "aria-hidden": "true" } }, icon("play", "icon filled run-play"), h("span", { class: "run-spinner" }), icon("stop", "icon filled run-stop")),
      h("span", { class: "run-label", text: "Run" }),
    );
    this.buildButton = iconButton(icon("hammer"), `Build (${shortcutLabel("Mod-Shift-b")})`, () => void this.buildActive());

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
        this.buildButton,
        this.runButton,
        h("span", { class: "titlebar-separator", attrs: { "aria-hidden": "true" } }),
        iconButton(icon("search"), `Open file (${shortcutLabel("Mod-p")})`, () => void this.quickOpen()),
        theme.button,
      ),
      h(
        "div",
        { class: "workspace" },
        h("aside", { class: "sidebar", attrs: { "aria-label": "Sidebar" } }, this.explorer.element, this.outline.element),
        resizer,
        h(
          "div",
          { class: "main-column" },
          h("main", { class: "editor-area" }, tabs.element, this.editorHost, this.welcome),
          this.panel.resizer,
          this.panel.element,
        ),
      ),
      this.statusBar.element,
    );
    const column = this.element.querySelector<HTMLElement>(".main-column");
    if (column) this.panel.mount(column);
    this.onRunStatus(this.runner.status);
    this.setupResizer(resizer);
    this.applySidebar();
    this.documents.subscribe(() => {
      this.showEditorOrWelcome();
      this.scheduleProblems();
    });
    this.showEditorOrWelcome();
    this.outline.update(null, 0);
    this.updateProblems();

    // Run keys are handled before the editor and terminals see them (the
    // editor binds Ctrl+Enter, and F5 would reload the page).
    window.addEventListener("keydown", (event) => this.onRunKey(event), true);
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
    if (update.docChanged || update.selectionSet) {
      this.scheduleOutline();
      this.updateCursor();
    }
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
    this.updateCursor();
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
      this.updateCursor();
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

  /**
   * Saves a document; resolves to whether it was saved. Saves of one document
   * run one after another: each sends the hash the one before received.
   */
  save(doc: OpenDocument): Promise<boolean> {
    const previous = this.saving.get(doc);
    const next = previous
      ? // Saved already by the save before, unless edited since.
        previous.then(() => (doc.disk === "in-sync" && !isDirty(doc) ? true : this.write(doc)))
      : this.write(doc);
    this.saving.set(doc, next);
    const done = () => {
      if (this.saving.get(doc) === next) this.saving.delete(doc);
    };
    next.then(done, done);
    return next;
  }

  private async write(doc: OpenDocument, overwrite = false): Promise<boolean> {
    const text = this.latest(doc).doc;
    try {
      if (doc.disk === "deleted") {
        await this.refreshTree();
        await this.createFolders(parentFolder(doc.path));
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
        return recreate ? this.write(doc) : false;
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
    if (answer === "overwrite") return this.write(doc, true);
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
    // Only the part that differs, so the cursor, scroll position, and folds
    // elsewhere stay put.
    const spec = { changes: textChange(current.doc.toString(), content) ?? [] };
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
        // A save in progress changes the file and then the hash; comparing
        // in between would look like a change by another program.
        if (this.saving.has(doc)) continue;
        const file = await this.api.readFile(doc.path);
        if (this.saving.has(doc) || this.documents.get(doc.path) !== doc) continue;
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
      await this.createFolders(parentFolder(path));
      await this.api.create(path, kind);
      this.explorer.expand(kind === "folder" ? path : parentFolder(path));
      await this.refreshTree();
      if (kind === "file") await this.openFile(path);
    } catch (error) {
      toast(`Couldn't create ${path}: ${message(error)}`, "error");
    }
  }

  /** Creates `folder` and the folders around it that the tree doesn't have. */
  private async createFolders(folder: string): Promise<void> {
    const parts = folder === "" ? [] : folder.split("/");
    for (let i = 1; i <= parts.length; i++) {
      const path = parts.slice(0, i).join("/");
      if (hasEntry(this.tree.entries, path)) continue;
      await this.api.create(path, "folder").catch((error: unknown) => {
        // Created by another program since the tree was read.
        if (!(error instanceof ApiError && error.conflict)) throw error;
      });
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
        const to = folder ? `${folder}/${value}` : value;
        if (entry.kind === "folder" && isWithin(to, entry.path)) return "A folder can't be moved into itself.";
        return nameProblem(value) ?? (hasEntry(this.tree.entries, to) ? "That name is taken." : null);
      },
      [0, entry.kind === "file" && dot > 0 ? dot : entry.name.length],
    );
    if (name === null || name === entry.name) return;
    const to = folder ? `${folder}/${name}` : name;
    try {
      // A name with folders moves the entry there, creating them as needed.
      await this.createFolders(parentFolder(to));
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
      this.runner.renamed(entry.path, to);
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
      this.runner.deleted(entry.path);
      await this.refreshTree();
    } catch (error) {
      toast(`Couldn't delete ${entry.path}: ${message(error)}`, "error");
    }
  }

  // Running, problems, and the status bar -------------------------------------------------

  /** Saves the active Vela file and runs it, showing Output. */
  async runActive(): Promise<void> {
    const doc = this.runnableDocument("run");
    if (!doc || this.runner.busy) return;
    // A program is one file, so only that file needs saving.
    if (isDirty(doc) && !(await this.save(doc))) return;
    this.panel.show("output", false);
    await nextFrame();
    await this.runner.run(doc.path);
  }

  /** Saves the active Vela file and builds it without running it. */
  async buildActive(): Promise<void> {
    const doc = this.runnableDocument("build");
    if (!doc || this.runner.busy) return;
    if (isDirty(doc) && !(await this.save(doc))) return;
    const built = await this.runner.build(doc.path);
    if (built) toast(`${fileName(doc.path)} built without errors.`, "success");
    // While a program runs, a failed build doesn't take over Output.
    else if (built === false && this.runner.status.kind !== "failed") this.panel.show("problems", false);
  }

  private runnableDocument(verb: string): OpenDocument | null {
    const doc = this.documents.current;
    if (!doc || !doc.path.endsWith(".vela")) {
      toast(`Open a .vela file to ${verb} it.`);
      return null;
    }
    return doc;
  }

  private onRunStatus(status: RunStatus): void {
    this.statusBar.setRun(status);
    const state = status.kind === "building" ? "building" : status.kind === "running" ? "running" : "idle";
    if (this.runButton.dataset["state"] !== state) {
      this.runButton.dataset["state"] = state;
      const [label, title] =
        state === "building" ? ["Building", "Building…"] : state === "running" ? ["Stop", `Stop (${shortcutLabel("Shift-F5")})`] : ["Run", `Run (${shortcutLabel("F5")})`];
      const labelElement = this.runButton.querySelector(".run-label");
      if (labelElement) labelElement.textContent = label;
      this.runButton.title = title;
      this.runButton.setAttribute("aria-label", title);
    }
    this.runButton.setAttribute("aria-busy", String(state === "building"));
    this.buildButton.disabled = state === "building";
    if (status.kind === "failed") {
      // Vela errors and a missing main point into the file; a C compiler's
      // failure is best read in Output.
      const inFile = status.failure.kind === "vela" || status.failure.kind === "no-main";
      this.panel.show(inFile ? "problems" : "output", false);
    }
  }

  private onRunKey(event: KeyboardEvent): void {
    if (event.altKey) return;
    if (document.querySelector("dialog[open]")) {
      // Not while a dialog asks something, but F5 mustn't reload the page either.
      if (event.key === "F5") event.preventDefault();
      return;
    }
    const mod = event.ctrlKey || event.metaKey;
    const key = event.key.toLowerCase();
    let action: (() => void) | null = null;
    if (event.key === "F5" && !mod) {
      action = event.shiftKey ? () => this.runner.stop() : () => void this.runActive();
    } else if (mod && !event.shiftKey && event.key === "Enter" && !(event.target instanceof HTMLInputElement)) {
      // Text fields keep theirs (such as replace all in the search panel).
      action = () => void this.runActive();
    } else if (mod && event.shiftKey && key === "b") {
      action = () => void this.buildActive();
    } else if (mod && !event.shiftKey && key === "j") {
      action = () => this.togglePanel();
    } else if (mod && event.shiftKey && key === "m") {
      action = () => this.panel.show("problems");
    } else if (event.ctrlKey && !event.metaKey && !event.shiftKey && event.code === "Backquote") {
      action = () => this.toggleTerminal();
    }
    if (!action) return;
    event.preventDefault();
    event.stopPropagation();
    action();
  }

  private togglePanel(): void {
    this.panel.toggle();
    if (!this.panel.isVisible) this.focusEditor();
  }

  private toggleTerminal(): void {
    const focused = this.shell.view.element.contains(document.activeElement);
    if (this.panel.isVisible && this.panel.activeTab === "terminal" && focused) {
      this.panel.hide();
      this.focusEditor();
    } else {
      this.panel.show("terminal");
    }
  }

  private focusEditor(): void {
    if (this.documents.current) this.view.focus();
  }

  private scheduleProblems(): void {
    clearTimeout(this.problemsTimer);
    // A little after typing pauses, so errors that come and go mid-word don't flicker.
    this.problemsTimer = setTimeout(() => this.updateProblems(), 300);
  }

  private updateProblems(): void {
    const live = this.documents.all
      .filter((doc) => doc.path.endsWith(".vela"))
      .map((doc) => ({ path: doc.path, diagnostics: this.analyzer.file(this.latest(doc).doc).diagnostics }));
    const files = collectProblems(live, this.runner.failures);
    this.problems.update(files);
    const count = problemCount(files);
    this.panel.setCount(count);
    this.statusBar.setProblems(count);
  }

  /** Opens a problem's file with the problem selected. */
  private async openProblem(problem: Problem): Promise<void> {
    await this.openFile(problem.path);
    if (this.documents.current?.path !== problem.path) return;
    const text = this.view.state.doc;
    let from = 0;
    let to = 0;
    if (problem.from !== null && problem.to !== null) {
      from = Math.min(problem.from, text.length);
      to = Math.min(Math.max(problem.to, from), text.length);
    } else if (problem.line !== null) {
      const line = text.line(Math.max(1, Math.min(problem.line, text.lines)));
      from = to = line.from + Math.max(0, Math.min((problem.col ?? 1) - 1, line.length));
    }
    this.view.dispatch({
      selection: { anchor: from, head: to },
      effects: EditorView.scrollIntoView(from, { y: "center" }),
      userEvent: "select.problem",
    });
    this.view.focus();
  }

  private updateCursor(): void {
    const doc = this.documents.current;
    if (!doc) {
      this.statusBar.setCursor(null);
      return;
    }
    const { state } = this.view;
    const head = state.selection.main.head;
    const line = state.doc.lineAt(head);
    const selected = state.selection.ranges.reduce((total, range) => total + range.to - range.from, 0);
    // Columns count characters, as the compiler's do.
    const col = [...line.text.slice(0, head - line.from)].length + 1;
    this.statusBar.setCursor({ line: line.number, col, selected }, doc.path.endsWith(".vela") ? "Vela" : "Plain Text");
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
      ["F5", "Run the file"],
      ["Mod-s", "Save"],
      ["Ctrl-Space", "Complete"],
      ["F12", "Go to definition"],
      ["Mod-Shift-o", "Go to a declaration"],
      ["Mod-/", "Toggle comment"],
      ["Mod-f", "Find and replace"],
      ["Ctrl-`", "Terminal"],
      ["Mod-j", "Toggle the panel"],
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

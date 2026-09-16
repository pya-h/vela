// The panel below the editor: Terminal, Output, and Problems tabs. It is
// resized by dragging its top edge (or with arrow keys there), hidden and shown
// with Ctrl+J, and remembers its height, tab, and visibility.

import { h, iconButton, shortcutLabel } from "../workbench/dom";
import { icon } from "../workbench/icons";
import { isBoolean, load, save } from "../workbench/storage";

export type PanelTab = "terminal" | "output" | "problems";

const TABS: { id: PanelTab; label: string }[] = [
  { id: "problems", label: "Problems" },
  { id: "output", label: "Output" },
  { id: "terminal", label: "Terminal" },
];

const HEIGHT = { min: 100, initial: 260 };
/** The editor's smallest height beside the panel. */
const EDITOR_MIN = 120;

export interface PanelContent {
  element: HTMLElement;
  /** Buttons shown in the header while the tab is active. */
  actions: HTMLElement[];
  /** Called when the tab is shown (after layout), and whether to focus it. */
  shown(focus: boolean): void;
}

const isTab = (value: unknown): value is PanelTab => value === "terminal" || value === "output" || value === "problems";
const isHeight = (value: unknown): value is number => typeof value === "number" && Number.isFinite(value) && value >= HEIGHT.min;

export class Panel {
  readonly element: HTMLElement;
  readonly resizer: HTMLElement;
  private readonly buttons = new Map<PanelTab, HTMLButtonElement>();
  private readonly badge: HTMLElement;
  private readonly actions: HTMLElement;
  private column: HTMLElement | null = null;
  private tab: PanelTab = load("panel-tab", "problems", isTab);
  private visible = load("panel-visible", false, isBoolean);
  private height = load("panel-height", HEIGHT.initial, isHeight);
  private count = 0;

  /** Called after the panel's own hide button hid it (so focus can go somewhere useful). */
  onHidden: () => void = () => {};

  constructor(private readonly contents: Record<PanelTab, PanelContent>) {
    this.badge = h("span", { class: "panel-badge", attrs: { "aria-hidden": "true" } });
    this.actions = h("div", { class: "panel-actions" });
    const tabs = h("div", { class: "panel-tabs", attrs: { role: "tablist", "aria-label": "Panel" } });
    for (const { id, label } of TABS) {
      const button = h(
        "button",
        {
          class: "panel-tab",
          attrs: { type: "button", role: "tab", id: `panel-tab-${id}`, "aria-controls": `panel-view-${id}` },
          on: {
            click: () => this.show(id, false),
            keydown: (event) => this.onTabKey(event, id),
          },
        },
        h("span", { text: label }),
        id === "problems" ? this.badge : null,
      );
      this.buttons.set(id, button);
      tabs.append(button);
    }
    for (const { id } of TABS) {
      const view = contents[id].element;
      view.classList.add("panel-view");
      view.id = `panel-view-${id}`;
      view.setAttribute("role", "tabpanel");
      view.setAttribute("aria-labelledby", `panel-tab-${id}`);
    }
    this.element = h(
      "section",
      { class: "panel", attrs: { "aria-label": "Panel" } },
      h(
        "header",
        { class: "panel-header" },
        tabs,
        h("div", { class: "titlebar-spacer" }),
        this.actions,
        iconButton(
          icon("chevronDown"),
          `Hide the panel (${shortcutLabel("Mod-j")})`,
          () => {
            this.hide();
            this.onHidden();
          },
          "icon-button small",
        ),
      ),
      h("div", { class: "panel-body" }, ...TABS.map(({ id }) => contents[id].element)),
    );
    this.resizer = h("div", {
      class: "panel-resizer",
      title: "Drag to resize; double-click to reset",
      attrs: { role: "separator", "aria-orientation": "horizontal", "aria-label": "Resize panel", tabindex: "0" },
    });
    this.setupResizer();
    this.setCount(0);
  }

  get isVisible(): boolean {
    return this.visible;
  }

  get activeTab(): PanelTab {
    return this.tab;
  }

  /** Attaches the panel to the column holding the editor above it, and restores it. */
  mount(column: HTMLElement): void {
    this.column = column;
    // A remembered height may not fit a smaller window.
    window.addEventListener("resize", () => this.apply());
    this.apply();
    if (this.visible) requestAnimationFrame(() => this.show(this.tab, false));
  }

  /** Shows the panel on `tab`. */
  show(tab: PanelTab, focus = true): void {
    this.tab = tab;
    this.visible = true;
    save("panel-tab", tab);
    save("panel-visible", true);
    this.apply();
    // After layout, so a terminal can measure itself.
    requestAnimationFrame(() => this.contents[tab].shown(focus));
  }

  hide(): void {
    this.visible = false;
    save("panel-visible", false);
    this.apply();
  }

  toggle(): void {
    if (this.visible) this.hide();
    else this.show(this.tab);
  }

  /** The number on the Problems tab, which pulses when it changes. */
  setCount(count: number): void {
    const changed = count !== this.count;
    this.count = count;
    this.badge.textContent = String(count);
    this.badge.hidden = count === 0;
    this.buttons.get("problems")?.setAttribute("aria-label", count === 1 ? "Problems (1 problem)" : `Problems (${count} problems)`);
    if (changed && count > 0) {
      this.badge.classList.remove("pulse");
      // Restart the animation.
      void this.badge.offsetWidth;
      this.badge.classList.add("pulse");
    }
  }

  private apply(): void {
    if (!this.column) return;
    this.column.classList.toggle("panel-hidden", !this.visible);
    this.column.style.setProperty("--panel-height", `${this.fittedHeight()}px`);
    // The resize handle's value, for assistive technology.
    this.resizer.setAttribute("aria-valuemin", String(HEIGHT.min));
    this.resizer.setAttribute("aria-valuemax", String(Math.max(HEIGHT.min, this.column.clientHeight - 120)));
    this.resizer.setAttribute("aria-valuenow", String(this.fittedHeight()));
    for (const { id } of TABS) {
      const active = id === this.tab;
      const button = this.buttons.get(id);
      button?.classList.toggle("active", active);
      button?.setAttribute("aria-selected", String(active));
      button?.setAttribute("tabindex", active ? "0" : "-1");
      this.contents[id].element.hidden = !active;
    }
    this.actions.replaceChildren(...this.contents[this.tab].actions);
  }

  /** The height, leaving the editor above at least a few lines. */
  private fittedHeight(): number {
    const room = (this.column?.clientHeight ?? 0) - EDITOR_MIN;
    return room > HEIGHT.min ? Math.min(this.height, room) : HEIGHT.min;
  }

  private onTabKey(event: KeyboardEvent, id: PanelTab): void {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const index = TABS.findIndex((tab) => tab.id === id) + (event.key === "ArrowRight" ? 1 : -1);
    const next = TABS[(index + TABS.length) % TABS.length];
    if (!next) return;
    this.show(next.id, false);
    this.buttons.get(next.id)?.focus();
  }

  private setHeight(height: number): void {
    const max = Math.max(HEIGHT.min, (this.column?.clientHeight ?? 600) - 120);
    this.height = Math.round(Math.max(HEIGHT.min, Math.min(max, height)));
    save("panel-height", this.height);
    this.apply();
  }

  private setupResizer(): void {
    const resizer = this.resizer;
    resizer.addEventListener("pointerdown", (event) => {
      if (!this.visible || !this.column) return;
      resizer.setPointerCapture(event.pointerId);
      const column = this.column;
      column.classList.add("resizing");
      const startY = event.clientY;
      const startHeight = this.height;
      const move = (e: PointerEvent) => this.setHeight(startHeight - (e.clientY - startY));
      const end = () => {
        column.classList.remove("resizing");
        resizer.removeEventListener("pointermove", move);
        resizer.removeEventListener("pointerup", end);
        resizer.removeEventListener("pointercancel", end);
      };
      resizer.addEventListener("pointermove", move);
      resizer.addEventListener("pointerup", end);
      resizer.addEventListener("pointercancel", end);
    });
    resizer.addEventListener("dblclick", () => this.setHeight(HEIGHT.initial));
    resizer.addEventListener("keydown", (event) => {
      if (event.key === "ArrowUp") this.setHeight(this.height + 16);
      else if (event.key === "ArrowDown") this.setHeight(this.height - 16);
      else return;
      event.preventDefault();
    });
  }
}

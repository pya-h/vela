// Light and dark themes: the system's choice until the user picks one.

import { iconButton } from "./dom";
import { icon } from "./icons";
import { load, save } from "./storage";

export type ThemeName = "dark" | "light";

const isTheme = (value: unknown): value is ThemeName | null => value === "dark" || value === "light" || value === null;

export class ThemeSwitch {
  readonly button: HTMLButtonElement;
  private chosen: ThemeName | null = load("theme", null, isTheme);
  private readonly system = window.matchMedia("(prefers-color-scheme: light)");

  constructor() {
    this.button = iconButton(icon("sun"), "Toggle theme", () => this.toggle());
    this.system.addEventListener("change", () => this.apply());
    this.apply();
  }

  get current(): ThemeName {
    return this.chosen ?? (this.system.matches ? "light" : "dark");
  }

  toggle(): void {
    this.chosen = this.current === "dark" ? "light" : "dark";
    save("theme", this.chosen);
    this.apply();
  }

  private apply(): void {
    const theme = this.current;
    document.documentElement.dataset["theme"] = theme;
    const next = theme === "dark" ? "light" : "dark";
    this.button.replaceChildren(icon(theme === "dark" ? "sun" : "moon"));
    this.button.title = `Switch to the ${next} theme`;
    this.button.setAttribute("aria-label", this.button.title);
  }
}

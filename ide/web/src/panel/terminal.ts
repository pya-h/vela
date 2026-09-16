// A terminal view: xterm.js sized to its element, in the editor's fonts and
// the current theme's colors. Links open with Ctrl/⌘+click.

import "@xterm/xterm/css/xterm.css";

import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { type ITheme, Terminal } from "@xterm/xterm";

import type { TerminalSize } from "../api/types";
import { h, hasShortcutModifier, isMac } from "../workbench/dom";

const FONT = '"JetBrains Mono", ui-monospace, "SFMono-Regular", Menlo, monospace';
/** How long to wait after the last layout change before telling the process its size. */
const RESIZE_DELAY = 80;

/** xterm.js theme keys and the CSS variables (`--terminal-…`, see styles/panel.css) they come from. */
const THEME_VARIABLES = {
  background: "bg",
  foreground: "fg",
  cursor: "cursor",
  cursorAccent: "bg",
  selectionBackground: "selection",
  black: "black",
  red: "red",
  green: "green",
  yellow: "yellow",
  blue: "blue",
  magenta: "magenta",
  cyan: "cyan",
  white: "white",
  brightBlack: "bright-black",
  brightRed: "bright-red",
  brightGreen: "bright-green",
  brightYellow: "bright-yellow",
  brightBlue: "bright-blue",
  brightMagenta: "bright-magenta",
  brightCyan: "bright-cyan",
  brightWhite: "bright-white",
  scrollbarSliderBackground: "scrollbar",
  scrollbarSliderHoverBackground: "scrollbar-hover",
  scrollbarSliderActiveBackground: "scrollbar-hover",
} as const satisfies Partial<Record<keyof ITheme, string>>;

/** The current theme's terminal colors; any the stylesheet doesn't define keep xterm.js's default. */
function currentTheme(): ITheme {
  const style = getComputedStyle(document.documentElement);
  const entries = Object.entries(THEME_VARIABLES)
    .map(([key, name]) => [key, style.getPropertyValue(`--terminal-${name}`).trim()] as const)
    .filter(([, value]) => value !== "");
  return Object.fromEntries(entries) as ITheme;
}

/** Ctrl/⌘+S, Ctrl/⌘+Shift+S, Ctrl/⌘+P, and Ctrl/⌘+Shift+O, which the workbench handles. */
/** Opens a link from the terminal, if Ctrl or ⌘ is held and it is a web address. */
export function openLink(event: MouseEvent, uri: string, open: (uri: string) => void = (u) => void window.open(u, "_blank", "noopener,noreferrer")): boolean {
  if (!(event.ctrlKey || event.metaKey) || !/^https?:\/\//i.test(uri)) return false;
  open(uri);
  return true;
}

export function isWorkbenchShortcut(event: KeyboardEvent, mac = isMac): boolean {
  if (!hasShortcutModifier(event, mac) || event.altKey) return false;
  const key = event.key.toLowerCase();
  return key === "s" || (key === "p" && !event.shiftKey) || (key === "o" && event.shiftKey);
}

export interface TerminalViewOptions {
  /** The accessible name, such as "Terminal". */
  label: string;
  /** Keystrokes and pastes. */
  onData(data: string): void;
  /** The terminal's size changed (after layout settles). */
  onResize(size: TerminalSize): void;
}

export class TerminalView {
  readonly element: HTMLElement;
  readonly terminal: Terminal;
  private readonly fitAddon = new FitAddon();
  private opening: Promise<void> | null = null;
  private resizeTimer: ReturnType<typeof setTimeout> | undefined;
  private reported = "";

  constructor(private readonly options: TerminalViewOptions) {
    this.element = h("div", { class: "terminal-host", attrs: { "aria-label": options.label } });
    this.terminal = new Terminal({
      fontFamily: FONT,
      fontSize: 13,
      lineHeight: 1.2,
      scrollback: 5000,
      cursorStyle: "bar",
      cursorInactiveStyle: "none",
      allowProposedApi: false,
      theme: currentTheme(),
      // Links a program writes with the terminal's hyperlink sequence open like
      // the addresses found in its text: with Ctrl or ⌘, and only for web addresses.
      linkHandler: { activate: (event, uri) => openLink(event, uri) },
    });
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.loadAddon(
      new WebLinksAddon((event, uri) => openLink(event, uri)),
    );
    // Saving and the pickers work from a terminal too: xterm.js would send
    // these keys to the program (Ctrl+S would even pause its output).
    this.terminal.attachCustomKeyEventHandler((event) => !isWorkbenchShortcut(event));
    this.terminal.onData((data) => this.options.onData(data));
    new MutationObserver(() => {
      this.terminal.options.theme = currentTheme();
    }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  }

  /**
   * Puts the terminal in its element, once the element is in the page and the
   * font has loaded (xterm.js measures the font when it opens).
   */
  open(): Promise<void> {
    this.opening ??= (async () => {
      await Promise.all([document.fonts.load(`13px ${FONT}`), document.fonts.load(`600 13px ${FONT}`)]).catch(() => undefined);
      this.terminal.options.theme = currentTheme();
      this.terminal.open(this.element);
      new ResizeObserver(() => this.layout()).observe(this.element);
      this.fit();
    })();
    return this.opening;
  }

  /** Fits the terminal to its element, if the element is shown, and returns the size. */
  fit(): TerminalSize {
    if (this.element.offsetWidth > 0 && this.element.offsetHeight > 0 && this.fitAddon.proposeDimensions()) {
      this.fitAddon.fit();
    }
    return { cols: this.terminal.cols, rows: this.terminal.rows };
  }

  write(data: string | Uint8Array): void {
    this.terminal.write(data);
  }

  focus(): void {
    this.terminal.focus();
  }

  private layout(): void {
    const size = this.fit();
    clearTimeout(this.resizeTimer);
    this.resizeTimer = setTimeout(() => {
      const key = `${size.cols}x${size.rows}`;
      if (key === this.reported) return;
      this.reported = key;
      this.options.onResize(size);
    }, RESIZE_DELAY);
  }
}

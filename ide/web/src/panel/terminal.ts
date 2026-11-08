import "@xterm/xterm/css/xterm.css";

import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { type ITheme, Terminal } from "@xterm/xterm";

import type { TerminalSize } from "../api/types";
import { h, hasShortcutModifier, isMac } from "../workbench/dom";

const FONT = '"JetBrains Mono", ui-monospace, "SFMono-Regular", Menlo, monospace';
const RESIZE_DELAY = 80;

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

function currentTheme(): ITheme {
  const style = getComputedStyle(document.documentElement);
  const entries = Object.entries(THEME_VARIABLES)
    .map(([key, name]) => [key, style.getPropertyValue(`--terminal-${name}`).trim()] as const)
    .filter(([, value]) => value !== "");
  return Object.fromEntries(entries) as ITheme;
}

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
  label: string;
  onData(data: string): void;
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
      linkHandler: { activate: (event, uri) => openLink(event, uri) },
    });
    this.terminal.loadAddon(this.fitAddon);
    this.terminal.loadAddon(
      new WebLinksAddon((event, uri) => openLink(event, uri)),
    );
    // Keep save/picker shortcuts out of the PTY (Ctrl+S pauses terminal output).
    this.terminal.attachCustomKeyEventHandler((event) => !isWorkbenchShortcut(event));
    this.terminal.onData((data) => this.options.onData(data));
    new MutationObserver(() => {
      this.terminal.options.theme = currentTheme();
    }).observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
  }

  open(): Promise<void> {
    this.opening ??= (async () => {
      // Wait for fonts so xterm.js measures final cell dimensions.
      await Promise.all([document.fonts.load(`13px ${FONT}`), document.fonts.load(`600 13px ${FONT}`)]).catch(() => undefined);
      this.terminal.options.theme = currentTheme();
      this.terminal.open(this.element);
      new ResizeObserver(() => this.layout()).observe(this.element);
      this.fit();
    })();
    return this.opening;
  }

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

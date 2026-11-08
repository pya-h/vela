type Child = Node | string | null | undefined | false;

interface Props {
  class?: string;
  title?: string;
  text?: string;
  attrs?: Record<string, string>;
  on?: { [E in keyof HTMLElementEventMap]?: (event: HTMLElementEventMap[E]) => void };
}

export function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...children: Child[]): HTMLElementTagNameMap[K] {
  const element = document.createElement(tag);
  if (props.class) element.className = props.class;
  if (props.title) element.title = props.title;
  if (props.text !== undefined) element.textContent = props.text;
  for (const [name, value] of Object.entries(props.attrs ?? {})) {
    element.setAttribute(name, value);
  }
  for (const [name, listener] of Object.entries(props.on ?? {})) {
    element.addEventListener(name, listener as EventListener);
  }
  for (const child of children) {
    if (child !== null && child !== undefined && child !== false) element.append(child);
  }
  return element;
}

export function iconButton(icon: SVGSVGElement, label: string, onClick: (event: MouseEvent) => void, className = "icon-button"): HTMLButtonElement {
  return h("button", { class: className, title: label, attrs: { type: "button", "aria-label": label }, on: { click: onClick } }, icon);
}

export const isMac = typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);

export function hasShortcutModifier(event: KeyboardEvent, mac = isMac): boolean {
  return mac ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}

export function shortcutLabel(key: string): string {
  const parts = key.split("-");
  const names = parts.map((part) => {
    switch (part) {
      case "Mod":
        return isMac ? "⌘" : "Ctrl";
      case "Ctrl":
        return isMac ? "⌃" : "Ctrl";
      case "Shift":
        return isMac ? "⇧" : "Shift";
      case "Alt":
        return isMac ? "⌥" : "Alt";
      default:
        return part.length === 1 ? part.toUpperCase() : part;
    }
  });
  return names.join(isMac ? "" : "+");
}

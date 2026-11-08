import { h } from "./dom";

export interface MenuItem {
  label: string;
  shortcut?: string;
  danger?: boolean;
  run: () => void;
}

let openMenu: HTMLElement | null = null;
let cleanup: (() => void) | null = null;

export function closeMenu(): void {
  openMenu?.remove();
  openMenu = null;
  cleanup?.();
  cleanup = null;
}

export function showMenu(x: number, y: number, items: (MenuItem | "separator")[]): void {
  closeMenu();
  const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const buttons: HTMLButtonElement[] = [];
  const menu = h(
    "div",
    { class: "menu", attrs: { role: "menu" } },
    ...items.map((item) => {
      if (item === "separator") return h("div", { class: "menu-separator", attrs: { role: "separator" } });
      const button = h(
        "button",
        {
          class: `menu-item${item.danger ? " danger" : ""}`,
          attrs: { type: "button", role: "menuitem" },
          on: {
            click: () => {
              closeMenu();
              item.run();
            },
          },
        },
        h("span", { text: item.label }),
        item.shortcut ? h("span", { class: "menu-shortcut", text: item.shortcut }) : null,
      );
      buttons.push(button);
      return button;
    }),
  );
  menu.addEventListener("keydown", (event) => {
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === "Escape") {
      closeMenu();
      opener?.focus();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : buttons.length - 1;
      buttons[(index + step) % buttons.length]?.focus();
    }
  });
  document.body.append(menu);
  // Clamp to viewport through the CSSOM to satisfy CSP.
  const { width, height } = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(x, window.innerWidth - width - 4)}px`;
  menu.style.top = `${Math.min(y, window.innerHeight - height - 4)}px`;
  openMenu = menu;
  buttons[0]?.focus();
  const dismiss = (event: Event) => {
    if (event instanceof MouseEvent && menu.contains(event.target as Node)) return;
    closeMenu();
  };
  window.addEventListener("mousedown", dismiss, true);
  window.addEventListener("blur", dismiss);
  cleanup = () => {
    window.removeEventListener("mousedown", dismiss, true);
    window.removeEventListener("blur", dismiss);
  };
}

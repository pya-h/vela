// Fixes from the review of the whole project: shortcuts in dialogs and
// terminals, the context menu, renaming a file to .vela, and building while a
// program runs.

import { cursorAtEndOf, expect, launch, openFile, tab, terminal, test, treeRow } from "./fixtures";

const FOREVER = "task main()\n    hold n = 0\n    loop true\n        n = n + 1\n    fi\nfi\n";

test("shortcuts inside a dialog do nothing, not the browser's action", async ({ page, studio }) => {
  await launch(page, studio);
  await page.evaluate(() => {
    window.addEventListener("keydown", (event) => {
      document.body.dataset["lastPrevented"] = `${event.key}:${event.defaultPrevented}`;
    });
  });
  await page.keyboard.press("Control+p");
  await expect(page.getByRole("dialog")).toBeVisible();
  for (const key of ["Control+s", "Control+j", "Control+p", "Control+Shift+B"]) {
    await page.keyboard.press(key);
    await expect(page.locator("body")).toHaveAttribute("data-last-prevented", /:true$/);
  }
  // Still the one picker, and no panel opened behind it.
  await expect(page.getByRole("dialog")).toHaveCount(1);
});

test("saving works while the terminal has focus", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 4);
  await page.keyboard.press("Enter");
  await page.keyboard.type("show(2)");
  await page.keyboard.press("Control+`");
  await expect(terminal(page, "terminal")).toContainText("$");
  await page.keyboard.press("Control+s");
  await expect(tab(page, "hello.vela")).not.toHaveClass(/dirty/);
  expect(studio.read("hello.vela")).toContain("show(2)");
});

test("the context menu works again after Escape", async ({ page, studio }) => {
  await launch(page, studio);
  await treeRow(page, "fib.vela").focus();
  await treeRow(page, "fib.vela").dispatchEvent("contextmenu");
  await expect(page.getByRole("menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  // Focus went back to the row.
  await expect(treeRow(page, "fib.vela")).toBeFocused();
  await treeRow(page, "fib.vela").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  await expect(page.getByRole("dialog")).toBeVisible();
});

test("a file renamed to .vela becomes a Vela file everywhere", async ({ page, studio }) => {
  studio.write("notes.txt", "task main()\n    show(1)\nfi\n");
  await launch(page, studio);
  await openFile(page, "notes.txt");
  await expect(page.locator(".statusbar")).toContainText("Plain Text");
  await treeRow(page, "notes.txt").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  await page.getByRole("dialog").getByRole("textbox").fill("notes.vela");
  await page.getByRole("dialog").getByRole("textbox").press("Enter");
  await expect(tab(page, "notes.vela")).toBeVisible();
  await expect(page.locator(".statusbar")).toContainText("Vela");
  await expect(page.locator(".statusbar")).not.toContainText("Plain Text");
  await expect(page.locator(".outline")).toContainText("main");
});

test("a build while a program runs keeps the Stop button and the run's result", async ({ page, studio }) => {
  studio.write("forever.vela", FOREVER);
  await launch(page, studio);
  await openFile(page, "forever.vela");
  await page.keyboard.press("F5");
  const run = page.locator(".run-button");
  await expect(run).toHaveAttribute("data-state", "running");

  // A slow build beside it.
  let release: () => void = () => {};
  const held = new Promise<void>((resolve) => (release = resolve));
  await page.route("**/api/build", async (route) => {
    await held;
    await route.continue();
  });
  await page.keyboard.press("Control+Shift+B");
  await expect(run).toHaveAttribute("data-state", "running");
  // The program can still be stopped, and its end is kept after the build.
  await page.keyboard.press("Shift+F5");
  await expect(page.locator(".status-run")).toContainText("forever.vela stopped");
  release();
  await expect(page.locator(".toast")).toContainText("built without errors");
  await expect(page.locator(".status-run")).toContainText("forever.vela stopped");
  await expect(run).toHaveAttribute("data-state", "idle");
});

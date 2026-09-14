// The workbench in Chromium against a real vela-studio: loading, files,
// editing and saving, and changes made on disk.

import { cursorAtEndOf, expect, launch, lineText, openFile, tab, test, treeRow } from "./fixtures";

test("loads the workspace and keeps the token out of the address", async ({ page, studio }) => {
  await launch(page, studio);
  expect(page.url()).not.toContain("token");
  await expect(page.locator(".tree-row")).toHaveCount(5);
  await expect(page.locator(".welcome")).toBeVisible();
  await expect(page).toHaveTitle(/· Vela Studio$/);

  // The theme choice and the session survive a reload.
  const theme = await page.locator("html").getAttribute("data-theme");
  await page.getByRole("button", { name: /Switch to the/ }).click();
  const toggled = theme === "dark" ? "light" : "dark";
  await expect(page.locator("html")).toHaveAttribute("data-theme", toggled);
  await page.reload();
  await expect(page.locator(".app")).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-theme", toggled);
});

test("without the token, the page explains how to open it", async ({ page, studio }) => {
  await page.goto(studio.url.replace(/#token=.*/, ""));
  await expect(page.getByRole("heading", { name: "Open Vela Studio from its link" })).toBeVisible();
});

test("edits and saves a file", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 4);
  await page.keyboard.press("Enter");
  expect(await lineText(page, 5)).toBe("    ");
  await page.keyboard.type('show("saved")');
  await expect(tab(page, "hello.vela")).toHaveClass(/dirty/);

  await page.keyboard.press("Control+s");
  await expect(tab(page, "hello.vela")).not.toHaveClass(/dirty/);
  expect(studio.read("hello.vela")).toBe('# Hello world in Vela\n\ntask main()\n    show("Hello, Vela!")\n    show("saved")\nfi\n');

  // Undoing back to the saved text shows no unsaved changes.
  await page.keyboard.type("x");
  await expect(tab(page, "hello.vela")).toHaveClass(/dirty/);
  await page.keyboard.press("Control+z");
  await expect(tab(page, "hello.vela")).not.toHaveClass(/dirty/);
});

test("closing a tab with unsaved changes asks first", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 1);
  await page.keyboard.type(" (edited)");
  await tab(page, "hello.vela").getByRole("button", { name: "Close hello.vela" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "Save changes?" })).toBeVisible();
  await dialog.getByRole("button", { name: "Cancel" }).click();
  await expect(tab(page, "hello.vela")).toBeVisible();

  await tab(page, "hello.vela").getByRole("button", { name: "Close hello.vela" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Don't Save" }).click();
  await expect(tab(page, "hello.vela")).toHaveCount(0);
  await expect(page.locator(".welcome")).toBeVisible();
  expect(studio.read("hello.vela")).not.toContain("(edited)");
});

test("saving over a file changed on disk asks before overwriting", async ({ page, studio }) => {
  // The refused save shows in the console as a failed request.
  studio.allowError(/status of 409/);
  await launch(page, studio);
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 1);
  await page.keyboard.type(" (mine)");
  studio.write("hello.vela", "# changed elsewhere\n");
  await page.keyboard.press("Control+s");
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("heading", { name: "File changed on disk" })).toBeVisible();
  await dialog.getByRole("button", { name: "Overwrite" }).click();
  await expect(tab(page, "hello.vela")).not.toHaveClass(/dirty/);
  expect(studio.read("hello.vela")).toContain("# Hello world in Vela (mine)");
});

test("a file changed on disk reloads when it has no unsaved changes", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "hello.vela");
  studio.write("hello.vela", "# rewritten\ntask main()\n    show(2)\nfi\n");
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(page.locator(".cm-content")).toContainText("# rewritten");
  await expect(page.locator(".toast")).toContainText("changed on disk and was reloaded");
  await expect(tab(page, "hello.vela")).not.toHaveClass(/dirty/);
});

test("creates, renames, and deletes files from the explorer", async ({ page, studio }) => {
  await launch(page, studio);

  // A new file in a new folder opens right away.
  await page.locator(".explorer").hover();
  await page.getByRole("button", { name: "New file", exact: true }).click();
  const input = page.getByRole("dialog").getByRole("textbox");
  await input.pressSequentially("lib/util");
  await input.press("Enter");
  await expect(tab(page, "lib/util.vela")).toHaveClass(/active/);
  await expect(treeRow(page, "lib/util.vela")).toBeVisible();
  expect(studio.read("lib/util.vela")).toBe("");

  // Renaming follows into the open tab.
  await treeRow(page, "lib/util.vela").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  const rename = page.getByRole("dialog").getByRole("textbox");
  await rename.fill("helpers.vela");
  await rename.press("Enter");
  await expect(tab(page, "lib/helpers.vela")).toBeVisible();
  await expect(treeRow(page, "lib/util.vela")).toHaveCount(0);

  // A taken name is refused in the dialog.
  await treeRow(page, "fib.vela").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Rename…" }).click();
  await page.getByRole("dialog").getByRole("textbox").fill("hello.vela");
  await expect(page.getByRole("dialog").getByRole("alert")).toHaveText("That name is taken.");
  await page.keyboard.press("Escape");

  // Deleting a folder closes its tabs.
  await treeRow(page, "lib").click({ button: "right" });
  await page.getByRole("menuitem", { name: "Delete" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  await expect(treeRow(page, "lib")).toHaveCount(0);
  await expect(tab(page, "lib/helpers.vela")).toHaveCount(0);
});

test("remembers open tabs across reloads", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "fib.vela");
  await openFile(page, "kinds.vela");
  await tab(page, "fib.vela").click();
  await page.reload();
  await expect(tab(page, "fib.vela")).toHaveClass(/active/);
  await expect(tab(page, "kinds.vela")).toBeVisible();
});

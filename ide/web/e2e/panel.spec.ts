// Running, building, problems, the terminal, and the status bar, against a
// real vela-studio that builds with the C compiler.

import { cursorAtEndOf, expect, launch, lineText, openFile, tab, terminal, test, treeRow } from "./fixtures";

const FOREVER = "task main()\n    hold n = 0\n    loop true\n        n = n + 1\n    fi\nfi\n";

test("runs the saved file and shows how it ended", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 4);
  await page.keyboard.press("Enter");
  await page.keyboard.type('show("and again")');
  await page.keyboard.press("F5");

  // Running saves the file first.
  await expect(page.locator(".panel-tab.active")).toHaveText("Output");
  await expect(terminal(page, "output")).toContainText("Hello, Vela!");
  await expect(terminal(page, "output")).toContainText("and again");
  await expect(terminal(page, "output")).toContainText(/── exited with code 0 · \d+ ms ──/);
  expect(studio.read("hello.vela")).toContain('show("and again")');
  await expect(tab(page, "hello.vela")).not.toHaveClass(/dirty/);
  await expect(page.locator(".status-run")).toContainText(/hello\.vela exited with code 0/);
  await expect(page.locator(".run-button")).toHaveAttribute("data-state", "idle");
  // The editor kept the focus.
  await expect(page.locator(".cm-content")).toBeFocused();
});

test("Ctrl+Enter runs and the Stop button stops", async ({ page, studio }) => {
  studio.write("forever.vela", FOREVER);
  await launch(page, studio);
  await openFile(page, "forever.vela");
  await cursorAtEndOf(page, 6);
  await page.keyboard.press("Control+Enter");
  const run = page.locator(".run-button");
  await expect(run).toHaveAttribute("data-state", "running");
  await expect(run).toHaveAccessibleName("Stop (Shift+F5)");
  await expect(page.locator(".status-run")).toContainText("Running forever.vela");
  // The editor's own Ctrl+Enter (insert a blank line) didn't run.
  expect(await page.locator(".cm-line").count()).toBe(7);

  await run.click();
  await expect(terminal(page, "output")).toContainText("── stopped ·");
  await expect(run).toHaveAttribute("data-state", "idle");
  await expect(page.locator(".status-run")).toContainText("forever.vela stopped");

  // Shift+F5 stops too.
  await page.keyboard.press("F5");
  await expect(run).toHaveAttribute("data-state", "running");
  await page.keyboard.press("Shift+F5");
  await expect(run).toHaveAttribute("data-state", "idle");

  // F5 in a dialog neither runs nor reloads the page. (Keys sent by the test
  // don't reach the browser's own shortcuts, so check that the page kept it.)
  await page.evaluate(() => {
    window.addEventListener("keydown", (event) => {
      if (event.key === "F5") document.body.dataset["f5Prevented"] = String(event.defaultPrevented);
    });
  });
  await page.keyboard.press("Control+p");
  await page.keyboard.press("F5");
  await expect(page.locator("body")).toHaveAttribute("data-f5-prevented", "true");
  await page.keyboard.press("Escape");
  await expect(page.locator(".status-run")).toContainText("forever.vela stopped");
  await expect(tab(page, "forever.vela")).toHaveClass(/active/);
});

test("problems come from editing and from builds", async ({ page, studio }) => {
  studio.write("helper.vela", "task helper()\n    show(1)\nfi\n");
  await launch(page, studio);

  // A live error: the status bar counts it, and choosing it selects it.
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 4);
  await page.keyboard.press("Enter");
  await page.keyboard.type("show(missing");
  await expect(page.locator(".statusbar")).toContainText("1 problem");
  await page.locator(".statusbar").getByRole("button", { name: "1 problem" }).click();
  await expect(page.locator(".panel-tab.active")).toContainText("Problems");
  await expect(page.locator(".panel-badge")).toHaveText("1");
  const problem = page.locator(".problem");
  await expect(problem).toHaveCount(1);
  await expect(problem).toContainText("unknown variable 'missing'");
  await expect(problem).toContainText("Ln 5, Col 10");
  await page.keyboard.press("Control+s");
  await cursorAtEndOf(page, 1);
  await problem.click();
  expect(await page.evaluate(() => window.getSelection()?.toString())).toBe("missing");

  // Running it shows the compiler's message in Output and the same problem once.
  await page.keyboard.press("F5");
  await expect(page.locator(".panel-tab.active")).toContainText("Problems");
  await expect(problem).toHaveCount(1);
  await expect(page.locator(".status-run")).toContainText("hello.vela: build failed");
  await page.locator(".panel-tab", { hasText: "Output" }).click();
  await expect(terminal(page, "output")).toContainText("hello.vela:5:10: error: unknown variable 'missing'");
  await expect(terminal(page, "output")).toContainText("── build failed ──");

  // A failure only the build finds: no main task.
  await treeRow(page, "helper.vela").click();
  await page.keyboard.press("F5");
  await expect(page.locator(".panel-tab.active")).toContainText("Problems");
  await expect(page.locator(".problem-file", { hasText: "helper.vela" })).toContainText("program has no 'main' task");
  await expect(page.locator(".panel-badge")).toHaveText("2");

  // Fixing both clears the list.
  await tab(page, "hello.vela").click();
  await cursorAtEndOf(page, 5);
  await page.keyboard.press("Shift+Home");
  await page.keyboard.type("show(1)");
  expect(await lineText(page, 5)).toBe("    show(1)");
  await tab(page, "helper.vela").click();
  await cursorAtEndOf(page, 3);
  await page.keyboard.press("Enter");
  await page.keyboard.type("task main()\n    helper()\nfi");
  await page.keyboard.press("F5");
  await expect(terminal(page, "output")).toContainText("exited with code 0");
  await expect(page.locator(".panel-badge")).toBeHidden();
  await expect(page.locator(".statusbar")).toContainText("No problems");
});

test("builds without running", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "fib.vela");
  await page.keyboard.press("Control+Shift+b");
  await expect(page.locator(".toast")).toContainText("fib.vela built without errors.");
  await expect(page.locator(".status-run")).toContainText("Built fib.vela");
});

test("the terminal runs a shell in the workspace", async ({ page, studio }) => {
  await launch(page, studio);
  // A slow start, so the first command is typed before the shell exists.
  await page.route("**/api/terminal", async (route) => {
    const response = await route.fetch();
    await new Promise((resolve) => setTimeout(resolve, 400));
    await route.fulfill({ response });
  });
  await page.keyboard.press("Control+Backquote");
  await expect(page.locator(".panel-tab.active")).toHaveText("Terminal");
  const shell = terminal(page, "terminal");
  // The terminal has the focus right away, even while its shell starts.
  await expect(page.locator("#panel-view-terminal textarea")).toBeFocused();
  await page.keyboard.type("ls; echo vela-$((6 * 7))\n");
  await expect(shell).toContainText("vela-42");
  await expect(shell).toContainText("hello.vela");

  await page.keyboard.type("exit 3\n");
  await expect(shell).toContainText("── exited with code 3");
  await expect(shell).toContainText("Press any key to start a new terminal.");
  await page.keyboard.press("Enter");
  await page.keyboard.type("echo again-$((1 + 1))\n");
  await expect(shell).toContainText("again-2");
  await expect(shell).not.toContainText("exited with code 3");

  // Ctrl+` from the terminal hides the panel and returns to the editor.
  await page.keyboard.press("Control+Backquote");
  await expect(page.locator(".main-column")).toHaveClass(/panel-hidden/);
});

test("the panel toggles and is remembered", async ({ page, studio }) => {
  await launch(page, studio);
  await expect(page.locator(".main-column")).toHaveClass(/panel-hidden/);
  await page.keyboard.press("Control+j");
  await expect(page.locator(".main-column")).not.toHaveClass(/panel-hidden/);
  await expect(page.locator(".panel-tab.active")).toHaveText(/^Problems/);
  await expect(page.locator(".panel")).toContainText("No problems in the open files.");
  // Arrow keys move between the tabs.
  await page.locator(".panel-tab", { hasText: "Problems" }).press("ArrowRight");
  await expect(page.locator(".panel-tab.active")).toHaveText("Output");
  await expect(page.locator(".panel-tab", { hasText: "Output" })).toBeFocused();
  await page.reload();
  await expect(page.locator(".app")).toBeVisible();
  await expect(page.locator(".main-column")).not.toHaveClass(/panel-hidden/);
  await expect(page.locator(".panel-tab.active")).toHaveText("Output");
  await page.keyboard.press("Control+Shift+m");
  await expect(page.locator(".panel-tab.active")).toHaveText(/^Problems/);
  await page.getByRole("button", { name: "Hide the panel (Ctrl+J)" }).click();
  await expect(page.locator(".main-column")).toHaveClass(/panel-hidden/);
});

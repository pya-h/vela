// Vela language support in the browser: highlighting, diagnostics,
// completion, hover, navigation, and the pickers.

import { cursorAtEndOf, expect, launch, lineText, openFile, tab, test } from "./fixtures";

test("highlights and outlines a Vela file", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "kinds.vela");
  const content = page.locator(".cm-content");
  await expect(content.locator(".tok-keyword", { hasText: /^kind$/ })).toHaveCount(1);
  await expect(content.locator(".tok-variant", { hasText: /^Circle$/ }).first()).toBeVisible();
  await expect(content.locator(".tok-comment")).toHaveText("# Kinds and pattern matching");
  await expect(content.locator(".tok-string").first()).toHaveText('"circle"');

  const outline = page.locator(".outline");
  await expect(outline.locator(".outline-row")).toHaveText([/Shape/, /Circle/, /radius/, /Rectangle/, /width/, /height/, /Dot/, /describe/, /main/]);
  await outline.locator(".outline-row", { hasText: "main" }).click();
  await expect(page.locator(".cm-activeLine")).toHaveText("task main()");
  await expect(outline.locator(".outline-row.current")).toContainText("main");
});

test("reports compiler errors as you type", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "hello.vela");
  await cursorAtEndOf(page, 4);
  await page.keyboard.press("Enter");
  await page.keyboard.type("show(missing");
  await expect(page.locator(".cm-lintRange-error")).toHaveText("missing");
  await expect(page.locator(".cm-lint-marker-error")).toBeVisible();
  await page.locator(".cm-lintRange-error").hover();
  await expect(page.locator(".cm-tooltip-lint")).toContainText("unknown variable 'missing'");
  // Fixing it clears the error; closing brackets were inserted automatically.
  expect(await lineText(page, 5)).toBe("    show(missing)");
  await page.keyboard.press("Control+Backspace");
  await page.keyboard.type("1");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
});

test("completes names and expands snippets", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "fib.vela");
  await cursorAtEndOf(page, 13);
  await page.keyboard.press("Enter");
  await page.keyboard.type("fi");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const option = (label: string) => popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: new RegExp(`^${label}$`) }) });
  await expect(option("fib")).toBeVisible();
  // Pick `fib`, which inserts a call with a field for its parameter.
  await option("fib").click();
  expect(await lineText(page, 14)).toBe("        fib(n)");
  await page.keyboard.type("i");
  expect(await lineText(page, 14)).toBe("        fib(i)");

  // A statement snippet, indented to the block.
  await page.keyboard.press("Escape");
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("whe");
  await option("when … else").click();
  await page.keyboard.type("i > 5");
  expect(await lineText(page, 15)).toBe("        when i > 5");
  expect(await lineText(page, 17)).toBe("        else");
  expect(await lineText(page, 19)).toBe("        fi");
});

test("hovers and goes to definitions", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "fib.vela");
  const call = page.locator(".cm-line").nth(12).locator(".tok-task", { hasText: "fib" });
  await call.hover();
  const hover = page.locator(".vs-hover");
  await expect(hover.locator(".vs-hover-code")).toHaveText("task fib(n: int) -> int");
  await expect(hover.locator(".vs-hover-code .tok-keyword").first()).toHaveText("task");

  await call.click();
  await page.keyboard.press("F12");
  await expect(page.locator(".cm-activeLine")).toHaveText("task fib(n: int) -> int");
  const selection = await page.evaluate(() => window.getSelection()?.toString());
  expect(selection).toBe("fib");
});

test("quick open and go to a declaration", async ({ page, studio }) => {
  await launch(page, studio);
  await page.keyboard.press("Control+p");
  const picker = page.locator(".palette");
  await picker.getByRole("combobox").pressSequentially("kin");
  await expect(picker.getByRole("option").first()).toContainText("kinds.vela");
  await page.keyboard.press("Enter");
  await expect(tab(page, "kinds.vela")).toHaveClass(/active/);

  await page.keyboard.press("Control+Shift+o");
  await picker.getByRole("combobox").pressSequentially("rect");
  await page.keyboard.press("Enter");
  await expect(page.locator(".cm-activeLine")).toContainText("Rectangle(width: float, height: float)");
});

const REGIONS =
  "shape Node\n    value: int\n    next: maybe ^Node\nfi\n\ntask push(r: region, list: maybe ^Node@r) -> ^Node@r\n    <- new Node(Node { value: 1, next: list }) in r\nfi\n\ntask main()\n    region nodes\n    bind first = push(nodes, none)\n    show(first^.value)\nfi\n";

test("regions: highlighting, hover, and completion of region names", async ({ page, studio }) => {
  studio.write("regions.vela", REGIONS);
  await launch(page, studio);
  await openFile(page, "regions.vela");
  const content = page.locator(".cm-content");
  const nodes = content.locator(".tok-region", { hasText: /^nodes$/ });
  await expect(nodes).toHaveCount(2);
  await nodes.first().hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("region nodes");

  // After the `in` of a `new`, the regions it can allocate in.
  await cursorAtEndOf(page, 13);
  await page.keyboard.press("Enter");
  await page.keyboard.type("bind two = new int(2) in ");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const nodesOption = popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: /^nodes$/ }) });
  await expect(nodesOption).toBeVisible();
  await nodesOption.click();
  expect(await lineText(page, 14)).toBe("    bind two = new int(2) in nodes");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);

  // In a call, a region parameter's argument offers only regions.
  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("bind three = push(");
  await page.keyboard.press("Control+Space");
  await expect(nodesOption).toBeVisible();
  await expect(popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: /^first$/ }) })).toHaveCount(0);
  await nodesOption.click();
  await page.keyboard.type(", none");
  expect(await lineText(page, 15)).toBe("    bind three = push(nodes, none)");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
});

const MAYBE = "task find(n: int) -> maybe int\n    when n > 0\n        <- n\n    fi\n    <- none\nfi\n\ntask main()\n    bind m = find(2)\n    show(m)\nfi\n";

test("maybe values: highlighting, hover, patterns, and plan B blocks", async ({ page, studio }) => {
  studio.write("maybe.vela", MAYBE);
  await launch(page, studio);
  await openFile(page, "maybe.vela");
  const content = page.locator(".cm-content");
  const maybe = content.locator(".tok-keyword", { hasText: /^maybe$/ });
  await expect(maybe).toHaveCount(1);
  await maybe.hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("maybe T");

  // After `is`, the patterns of the value's type.
  await cursorAtEndOf(page, 10);
  await page.keyboard.press("Enter");
  await page.keyboard.type("when m is ");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const option = (label: string) => popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: label }) });
  await expect(option("{value}")).toBeVisible();
  await expect(option("none")).toBeVisible();
  await page.keyboard.press("Escape");

  // A plan B's block is indented, and its `}` lines up with the line that opened it.
  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await page.keyboard.type("bind n = m {");
  await page.keyboard.press("Enter");
  await page.keyboard.type("0");
  expect(await lineText(page, 12)).toBe("        0");
  expect(await lineText(page, 13)).toBe("    }");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
});

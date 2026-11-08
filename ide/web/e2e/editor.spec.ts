import { cursorAtEndOf, expect, launch, lineText, openFile, tab, terminal, test, treeRow } from "./fixtures";

test("highlights and outlines a Vela file", async ({ page, studio }) => {
  await launch(page, studio);
  await openFile(page, "kinds.vela");
  const content = page.locator(".cm-content");
  await expect(content.locator(".tok-keyword", { hasText: /^kind$/ })).toHaveCount(1);
  await expect(content.locator(".tok-variant", { hasText: /^Circle$/ }).first()).toBeVisible();
  await expect(content.locator(".tok-comment")).toHaveText("; Kinds and pattern matching");
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
  await option("fib").click();
  expect(await lineText(page, 14)).toBe("        fib(n)");
  await page.keyboard.type("i");
  expect(await lineText(page, 14)).toBe("        fib(i)");

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
  "shape Node\n    value: int\n    next: maybe ^Node\nfi\n\ntask push(r: region, list: maybe ^Node@r) -> ^Node@r\n    <- new Node(Node { value: 1, next: list }) at r\nfi\n\ntask main()\n    region nodes\n    fixed first = push(nodes, none)\n    show(first^.value)\nfi\n";

test("regions: highlighting, hover, and completion of region names", async ({ page, studio }) => {
  studio.write("regions.vela", REGIONS);
  await launch(page, studio);
  await openFile(page, "regions.vela");
  const content = page.locator(".cm-content");
  const nodes = content.locator(".tok-region", { hasText: /^nodes$/ });
  await expect(nodes).toHaveCount(2);
  await nodes.first().hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("region nodes");

  await cursorAtEndOf(page, 13);
  await page.keyboard.press("Enter");
  await page.keyboard.type("fixed two = new int(2) at ");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const nodesOption = popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: /^nodes$/ }) });
  await expect(nodesOption).toBeVisible();
  await nodesOption.click();
  expect(await lineText(page, 14)).toBe("    fixed two = new int(2) at nodes");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);

  await page.keyboard.press("End");
  await page.keyboard.press("Enter");
  await page.keyboard.type("fixed three = push(");
  await page.keyboard.press("Control+Space");
  await expect(nodesOption).toBeVisible();
  await expect(popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: /^first$/ }) })).toHaveCount(0);
  await nodesOption.click();
  await page.keyboard.type(", none");
  expect(await lineText(page, 15)).toBe("    fixed three = push(nodes, none)");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
});

const MAYBE = "task find(n: int) -> maybe int\n    when n > 0\n        <- n\n    fi\n    <- none\nfi\n\ntask main()\n    fixed m = find(2)\n    show(m)\nfi\n";

test("maybe values: highlighting, hover, patterns, and plan B blocks", async ({ page, studio }) => {
  studio.write("maybe.vela", MAYBE);
  await launch(page, studio);
  await openFile(page, "maybe.vela");
  const content = page.locator(".cm-content");
  const maybe = content.locator(".tok-keyword", { hasText: /^maybe$/ });
  await expect(maybe).toHaveCount(1);
  await maybe.hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("maybe T");

  await cursorAtEndOf(page, 10);
  await page.keyboard.press("Enter");
  await page.keyboard.type("when m is ");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const option = (label: string) => popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: label }) });
  await expect(option("{value}")).toBeVisible();
  await expect(option("none")).toBeVisible();
  await page.keyboard.press("Escape");

  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await page.keyboard.type("fixed n = m {");
  await page.keyboard.press("Enter");
  await page.keyboard.type("0");
  expect(await lineText(page, 12)).toBe("        0");
  expect(await lineText(page, 13)).toBe("    }");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
});

const NAMED =
  "task score(hits: int, misses: int) -> total: int\n    total <- hits * 10\n    when misses == 0\n        <- total + 50\n    fi\n    total <- total - misses * 5\nfi\n\ntask main()\n    show(score(3, 1))\nfi\n";

test("named results: highlighting, hover, and setting them", async ({ page, studio }) => {
  studio.write("named.vela", NAMED);
  await launch(page, studio);
  await openFile(page, "named.vela");
  const content = page.locator(".cm-content");
  const header = content.locator(".cm-line").first();
  await expect(header.locator(".tok-variable", { hasText: /^total$/ })).toHaveCount(1);
  await header.locator(".tok-variable", { hasText: /^total$/ }).hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("result total: int");

  await cursorAtEndOf(page, 6);
  await page.keyboard.press("Enter");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const option = (label: string) => popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: new RegExp(`^${label}`) }) });
  await expect(option("total <- ")).toBeVisible();
  await option("total <- ").click();
  await page.keyboard.type("0");
  expect(await lineText(page, 7)).toBe("    total <- 0");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);

  await page.keyboard.press("Home");
  await page.keyboard.press("Shift+End");
  await page.keyboard.type("    total = 0");
  await expect(page.locator(".cm-lintRange-error")).toHaveText("total");
  await page.locator(".cm-lintRange-error").hover();
  await expect(page.locator(".cm-tooltip-lint")).toContainText("'total' is a result; set it with 'total <- …'");
});

const RESULTS =
  "task divmod(a: int, b: int) -> int, int\n    <- a / b, a % b\nfi\n\ntask main()\n    fixed q, r = divmod(17, 5)\n    show(q)\n    show(r)\nfi\n";

test("several results: highlighting, hover, and completion of the names", async ({ page, studio }) => {
  studio.write("results.vela", RESULTS);
  await launch(page, studio);
  await openFile(page, "results.vela");
  const content = page.locator(".cm-content");
  await expect(content.locator(".cm-line").first().locator(".tok-type-builtin", { hasText: /^int$/ })).toHaveCount(4);

  await content.locator(".tok-variable", { hasText: /^q$/ }).first().hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("fixed q: int");

  await cursorAtEndOf(page, 1);
  await page.keyboard.type(", ");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  const option = (label: string) => popup.getByRole("option").filter({ has: page.locator(".cm-completionLabel", { hasText: new RegExp(`^${label}$`) }) });
  await expect(option("bool")).toBeVisible();
  await expect(option("nothing")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Backspace");

  await cursorAtEndOf(page, 8);
  await page.keyboard.press("Enter");
  await page.keyboard.type("fixed a, ");
  await page.keyboard.press("Control+Space");
  await expect(popup).toHaveCount(0);
  await page.keyboard.type("b = divmod(9, 4)");
  expect(await lineText(page, 9)).toBe("    fixed a, b = divmod(9, 4)");
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
});

const GEOMETRY = "priv task twice(n: int) -> int\n    <- n * 2\nfi\n\ntask area(side: int) -> int\n    <- side * twice(side) / 2\nfi\n";
const INCLUDES_GEOMETRY = "include shapes/geometry\n\ntask main()\n    show(geometry.area(3))\nfi\n";

test("modules: highlighting, hover, definitions, errors, and running", async ({ page, studio }) => {
  studio.write("app/shapes/geometry.vela", GEOMETRY);
  studio.write("app/main.vela", INCLUDES_GEOMETRY);
  await launch(page, studio);
  await treeRow(page, "app").click();
  await openFile(page, "app/main.vela");
  const content = page.locator(".cm-content");
  await expect(content.locator(".tok-module", { hasText: /^geometry$/ })).toHaveCount(2);
  await expect(content.locator(".tok-task", { hasText: /^area$/ })).toHaveCount(1);
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
  await content.locator(".tok-task", { hasText: /^area$/ }).hover();
  await expect(page.locator(".vs-hover .vs-hover-code")).toHaveText("task geometry.area(side: int) -> int");
  await expect(page.locator(".vs-hover .vs-hover-doc")).toHaveText("From module geometry (app/shapes/geometry.vela).");

  await cursorAtEndOf(page, 4);
  await page.keyboard.press("Enter");
  await page.keyboard.type("show(geometry.");
  const popup = page.locator(".cm-tooltip-autocomplete");
  await expect(popup.locator(".cm-completionLabel")).toHaveText(["area"]);
  await page.keyboard.press("Escape");
  await page.keyboard.press("End");
  await page.keyboard.press("Shift+Home");
  await page.keyboard.press("Shift+Home");
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Backspace");
  expect(await lineText(page, 5)).toBe("fi");

  await content.locator(".tok-task", { hasText: /^area$/ }).click({ modifiers: ["ControlOrMeta"] });
  await expect(tab(page, "app/shapes/geometry.vela")).toHaveClass(/active/);
  expect(await page.evaluate(() => getSelection()?.toString())).toBe("area");

  await cursorAtEndOf(page, 2);
  await page.keyboard.type(" + missing");
  await tab(page, "app/main.vela").click();
  await expect(page.locator(".cm-lintRange-error")).toHaveText("shapes/geometry");
  await page.locator(".cm-lintRange-error").hover();
  await expect(page.locator(".cm-tooltip-lint")).toContainText("app/shapes/geometry.vela:2:16: unknown variable 'missing'");
  await page.mouse.move(0, 0);
  await expect(page.locator(".cm-tooltip-lint")).toHaveCount(0);

  await tab(page, "app/shapes/geometry.vela").click();
  for (let i = 0; i < " + missing".length; i++) await page.keyboard.press("Backspace");
  await page.keyboard.type(" * 1");
  await tab(page, "app/main.vela").click();
  await expect(page.locator(".cm-lintRange-error")).toHaveCount(0);
  await page.keyboard.press("F5");
  await expect(terminal(page, "output")).toContainText("9");
  await expect(terminal(page, "output")).toContainText(/── exited with code 0 · \d+ ms ──/);
  expect(studio.read("app/shapes/geometry.vela")).toContain("<- n * 2 * 1");
  await expect(tab(page, "app/shapes/geometry.vela")).not.toHaveClass(/dirty/);
});

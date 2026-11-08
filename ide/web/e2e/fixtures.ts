import { type ChildProcess, spawn } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { type Locator, type Page, test as base, expect } from "@playwright/test";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const binary = process.env["VELA_STUDIO_BIN"] ?? join(repo, "target", "debug", "vela-studio");

export interface Studio {
  url: string;
  workspace: string;
  read(name: string): string;
  write(name: string, content: string): void;
  remove(name: string): void;
  exists(name: string): boolean;
  errors: string[];
  allowError(pattern: RegExp): void;
}

function start(workspace: string): Promise<{ server: ChildProcess; url: string }> {
  return new Promise((resolve, reject) => {
    // Terminals run a plain shell, whatever the user's shell and its setup are.
    const env = { ...process.env, SHELL: "/bin/sh" };
    const server = spawn(binary, [workspace, "--no-open"], { stdio: ["ignore", "pipe", "pipe"], env });
    let output = "";
    const timer = setTimeout(() => reject(new Error(`vela-studio didn't start:\n${output}`)), 10_000);
    const onData = (data: Buffer) => {
      output += data.toString();
      const url = /http:\/\/127\.0\.0\.1:\d+\/#token=[0-9a-f]{64}/.exec(output)?.[0];
      if (url) {
        clearTimeout(timer);
        resolve({ server, url });
      }
    };
    server.stdout?.on("data", onData);
    server.stderr?.on("data", onData);
    server.on("error", reject);
  });
}

export const test = base.extend<{ studio: Studio }>({
  studio: async ({ page }, use) => {
    const workspace = mkdtempSync(join(tmpdir(), "vela-studio-e2e-"));
    const examples = join(repo, "examples");
    for (const name of readdirSync(examples)) {
      const from = join(examples, name);
      if (statSync(from).isDirectory()) cpSync(from, join(workspace, name), { recursive: true });
      else if (name.endsWith(".vela")) copyFileSync(from, join(workspace, name));
    }
    const { server, url } = await start(workspace);
    const errors: string[] = [];
    const allowed: RegExp[] = [];
    page.on("console", (message) => {
      if (message.type() === "error") errors.push(message.text());
    });
    page.on("pageerror", (error) => errors.push(error.message));
    await use({
      url,
      workspace,
      errors,
      allowError: (pattern) => void allowed.push(pattern),
      read: (name) => readFileSync(join(workspace, name), "utf8"),
      write: (name, content) => {
        mkdirSync(dirname(join(workspace, name)), { recursive: true });
        writeFileSync(join(workspace, name), content);
      },
      remove: (name) => rmSync(join(workspace, name), { recursive: true, force: true }),
      exists: (name) => existsSync(join(workspace, name)),
    });
    server.kill("SIGINT");
    rmSync(workspace, { recursive: true, force: true });
    expect(
      errors.filter((error) => !allowed.some((pattern) => pattern.test(error))),
      "errors logged by the page",
    ).toEqual([]);
  },
});

export { expect };

export async function launch(page: Page, studio: Studio): Promise<void> {
  await page.goto(studio.url);
  await expect(page.locator(".app")).toBeVisible();
}

export function treeRow(page: Page, path: string): Locator {
  return page.locator(`.tree-row[data-path="${path}"]`);
}

export function tab(page: Page, path: string): Locator {
  return page.locator(`.tab[data-path="${path}"]`);
}

export async function openFile(page: Page, path: string): Promise<void> {
  await treeRow(page, path).click();
  await expect(tab(page, path)).toHaveClass(/active/);
  await expect(page.locator(".cm-content")).toBeVisible();
}

export async function cursorAtEndOf(page: Page, line: number): Promise<void> {
  await page.locator(".cm-line").nth(line - 1).click();
  await page.keyboard.press("End");
}

export function terminal(page: Page, view: "output" | "terminal"): Locator {
  return page.locator(`#panel-view-${view} .xterm-rows`);
}

export function lineText(page: Page, line: number): Promise<string> {
  return page.locator(".cm-line").nth(line - 1).innerText();
}

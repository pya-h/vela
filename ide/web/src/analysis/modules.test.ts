import { describe, expect, it } from "vitest";

import { ModuleSources } from "./modules";

function reader(files: Record<string, string>, open: Record<string, string> = {}) {
  const reads: string[] = [];
  const waiting: (() => void)[] = [];
  return {
    reads,
    open,
    settle: async () => {
      for (const resolve of waiting.splice(0)) resolve();
      await new Promise((resolve) => setTimeout(resolve, 0));
    },
    reader: {
      read: (path: string) => {
        reads.push(path);
        return new Promise<string | null>((resolve) => waiting.push(() => resolve(files[path] ?? null)));
      },
      openText: (path: string) => open[path],
    },
  };
}

describe("ModuleSources", () => {
  it("fetches each asked-for file once, and notifies when it arrives", async () => {
    const files = reader({ "lib/a.vela": "task a()\nfi\n" });
    const modules = new ModuleSources();
    modules.connect(files.reader);
    let changes = 0;
    modules.onChange(() => changes++);
    modules.request(["lib/a.vela", "lib/missing.vela"]);
    modules.request(["lib/a.vela"]);
    expect(files.reads).toEqual(["lib/a.vela", "lib/missing.vela"]);
    expect(modules.sources()).toEqual({});
    const before = modules.version;
    await files.settle();
    expect(changes).toBe(2);
    expect(modules.version).toBeGreaterThan(before);
    expect(modules.sources()).toEqual({ "lib/a.vela": "task a()\nfi\n" });
    modules.request(["lib/missing.vela"]);
    expect(files.reads).toHaveLength(2);
  });

  it("uses an open file's current text, and changes version when it is edited", async () => {
    const files = reader({ "lib/a.vela": "on disk" }, { "lib/a.vela": "edited" });
    const modules = new ModuleSources();
    modules.connect(files.reader);
    modules.request(["lib/a.vela"]);
    expect(files.reads).toEqual([]);
    expect(modules.sources()).toEqual({ "lib/a.vela": "edited" });
    files.open["lib/a.vela"] = "edited again";
    const version = modules.version;
    modules.changed("lib/a.vela");
    expect(modules.version).toBe(version + 1);
    expect(modules.sources()).toEqual({ "lib/a.vela": "edited again" });
    modules.changed("main.vela");
    expect(modules.version).toBe(version + 1);
  });

  it("reads a file again once it exists, when told it changed", async () => {
    const disk: Record<string, string> = {};
    const files = reader(disk);
    const modules = new ModuleSources();
    modules.connect(files.reader);
    modules.request(["lib/new.vela"]);
    await files.settle();
    expect(modules.sources()).toEqual({});
    disk["lib/new.vela"] = "task n()\nfi\n";
    const version = modules.version;
    modules.changed("lib/new.vela");
    expect(modules.version).toBe(version + 1);
    modules.request(["lib/new.vela"]);
    await files.settle();
    expect(files.reads).toEqual(["lib/new.vela", "lib/new.vela"]);
    expect(modules.sources()).toEqual({ "lib/new.vela": "task n()\nfi\n" });
    // A file that was read isn't read again when it changes: its editor has its text.
    modules.changed("lib/new.vela");
    modules.request(["lib/new.vela"]);
    expect(files.reads).toHaveLength(2);
  });

  it("drops answers to requests made before it forgot", async () => {
    const files = reader({ "lib/a.vela": "old" });
    const modules = new ModuleSources();
    modules.connect(files.reader);
    let changes = 0;
    modules.onChange(() => changes++);
    modules.request(["lib/a.vela"]);
    modules.forget();
    // The analysis that asked is told to run again, or it would wait forever.
    expect(changes).toBe(1);
    await files.settle();
    expect(modules.sources()).toEqual({});
    modules.request(["lib/a.vela"]);
    await files.settle();
    expect(files.reads).toEqual(["lib/a.vela", "lib/a.vela"]);
    expect(modules.sources()).toEqual({ "lib/a.vela": "old" });
  });
});

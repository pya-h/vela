import { EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import { Documents, type OpenDocument, fileName, isDirty, isWithin, nameProblem, parentFolder, textChange } from "./documents";
import { fuzzyFilter, fuzzyMatch } from "./fuzzy";

function doc(path: string, content = "text"): OpenDocument {
  const state = EditorState.create({ doc: content });
  return { path, state, saved: state.doc, hash: "h0", disk: "in-sync", scrollTop: 0 };
}

describe("reloading", () => {
  it("changes only the part of the text that differs", () => {
    expect(textChange("same", "same")).toBeNull();
    expect(textChange("a\nb\nc\n", "a\nB!\nc\n")).toEqual({ from: 2, to: 3, insert: "B!" });
    expect(textChange("abc", "abcabc")).toEqual({ from: 3, to: 3, insert: "abc" });
    expect(textChange("abcabc", "abc")).toEqual({ from: 3, to: 6, insert: "" });
    expect(textChange("", "x")).toEqual({ from: 0, to: 0, insert: "x" });
    expect(textChange("aaa", "aa")).toEqual({ from: 2, to: 3, insert: "" });
    for (const [before, after] of [["hello world", "hello there world"], ["x😀y", "x😁y"], ["abba", "aba"]] as const) {
      const change = textChange(before, after)!;
      expect(before.slice(0, change.from) + change.insert + before.slice(change.to)).toBe(after);
    }
  });
});

describe("paths", () => {
  it("splits and compares workspace paths", () => {
    expect([fileName("a/b/c.vela"), fileName("c.vela")]).toEqual(["c.vela", "c.vela"]);
    expect([parentFolder("a/b/c.vela"), parentFolder("c.vela")]).toEqual(["a/b", ""]);
    expect([isWithin("a/b", "a"), isWithin("a", "a"), isWithin("ab", "a")]).toEqual([true, true, false]);
  });

  it("checks names for new and renamed entries", () => {
    expect(nameProblem("main.vela")).toBeNull();
    expect(nameProblem("lib/util.vela")).toBeNull();
    for (const bad of ["a//b", "../x", "./x", "a/", "a\\b", "C:x", "tab\there", ".hidden"]) {
      expect(nameProblem(bad), bad).not.toBeNull();
    }
  });
});

describe("Documents", () => {
  it("tracks unsaved changes against the saved text", () => {
    const docs = new Documents();
    const a = doc("a.vela", "one");
    docs.add(a);
    expect(isDirty(a)).toBe(false);
    a.state = a.state.update({ changes: { from: 3, insert: " two" } }).state;
    expect(isDirty(a)).toBe(true);
    a.state = a.state.update({ changes: { from: 3, to: 7 } }).state;
    expect(isDirty(a)).toBe(false);
    a.state = a.state.update({ changes: { from: 0, insert: "x" } }).state;
    docs.markSaved("a.vela", a.state.doc, "h1");
    expect([isDirty(a), a.hash]).toEqual([false, "h1"]);
    a.disk = "deleted";
    expect(isDirty(a)).toBe(true);
  });

  it("picks the neighbouring tab when the active one closes", () => {
    const docs = new Documents();
    let changes = 0;
    docs.subscribe(() => changes++);
    for (const path of ["a", "b", "c"]) docs.add(doc(path));
    docs.active = "b";
    expect(docs.remove("b")).toBe("c");
    docs.active = "c";
    expect(docs.remove("c")).toBe("a");
    expect(docs.remove("missing")).toBe("a");
    expect(docs.remove("a")).toBeNull();
    expect(docs.current).toBeNull();
    expect(changes).toBeGreaterThan(0);
  });

  it("follows renamed files and folders", () => {
    const docs = new Documents();
    for (const path of ["lib/a.vela", "lib/sub/b.vela", "library.vela", "c.vela"]) docs.add(doc(path));
    docs.active = "lib/sub/b.vela";
    docs.renamed("lib", "src");
    expect(docs.all.map((d) => d.path)).toEqual(["src/a.vela", "src/sub/b.vela", "library.vela", "c.vela"]);
    expect(docs.active).toBe("src/sub/b.vela");
    expect(docs.get("src/a.vela")?.path).toBe("src/a.vela");
    docs.renamed("c.vela", "d.vela");
    expect(docs.within("src").map((d) => d.path)).toEqual(["src/a.vela", "src/sub/b.vela"]);
    expect(docs.within("d.vela").map((d) => d.path)).toEqual(["d.vela"]);
  });
});

describe("fuzzy matching", () => {
  it("matches characters in order", () => {
    expect(fuzzyMatch("fb", "fib.vela")?.positions).toEqual([0, 2]);
    expect(fuzzyMatch("bf", "fib.vela")).toBeNull();
    expect(fuzzyMatch("", "anything")).toEqual({ score: 0, positions: [] });
    // Indices stay those of the text, even where lowercasing would lengthen it.
    expect(fuzzyMatch("x", "İx")?.positions).toEqual([1]);
    expect(fuzzyMatch("b", "😀b")?.positions).toEqual([2]);
  });

  it("prefers names, word starts, and runs", () => {
    const files = ["examples/hello.vela", "lib/help.vela", "hello.vela", "shell.vela"];
    expect(fuzzyFilter(files, "hel", (f) => f).map((r) => r.item)).toEqual(["hello.vela", "lib/help.vela", "examples/hello.vela", "shell.vela"]);
    expect(fuzzyFilter(files, "zz", (f) => f)).toEqual([]);
  });
});

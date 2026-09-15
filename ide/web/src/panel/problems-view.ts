// The Problems tab: problems grouped by file; choosing one opens the file at
// the problem.

import { fileName, parentFolder } from "../workbench/documents";
import { h } from "../workbench/dom";
import { icon } from "../workbench/icons";
import type { FileProblems, Problem } from "./problems";

export class ProblemsView {
  readonly element: HTMLElement;
  private rendered = "";

  constructor(private readonly open: (problem: Problem) => void) {
    this.element = h("div", { class: "problems", attrs: { role: "group", "aria-label": "Problems" } });
    this.update([]);
  }

  update(files: readonly FileProblems[]): void {
    const key = JSON.stringify(files);
    if (key === this.rendered) return;
    this.rendered = key;
    if (files.length === 0) {
      this.element.replaceChildren(h("p", { class: "panel-empty", text: "No problems in the open files." }));
      return;
    }
    this.element.replaceChildren(
      ...files.map((file) =>
        h(
          "section",
          { class: "problem-file" },
          h(
            "h3",
            { class: "problem-file-name", title: file.path },
            icon("file", "icon"),
            h("span", { text: fileName(file.path) }),
            parentFolder(file.path) ? h("span", { class: "problem-folder", text: parentFolder(file.path) }) : null,
            h("span", { class: "problem-count", text: String(file.problems.length) }),
          ),
          ...file.problems.map((problem) =>
            h(
              "button",
              {
                class: "problem",
                title: problem.source === "build" ? "Reported by the last build" : "Reported while editing",
                attrs: { type: "button" },
                on: { click: () => this.open(problem) },
              },
              icon("error", "icon problem-icon"),
              h("span", { class: "problem-message", text: problem.message }),
              h("span", { class: "problem-source", text: problem.source === "build" ? "build" : "vela" }),
              problem.line !== null ? h("span", { class: "problem-place", text: `Ln ${problem.line}, Col ${problem.col ?? 1}` }) : null,
            ),
          ),
        ),
      ),
    );
  }
}

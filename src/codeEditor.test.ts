import { describe, expect, it, vi } from "vitest";
import { highlightTree } from "@lezer/highlight";
import { syntaxTree } from "@codemirror/language";
import { createCodeEditor, languageFor } from "./codeEditor";

describe("languageFor", () => {
  it.each([
    ["main.tf", "HCL"],
    ["x.tofu", "HCL"],
    ["terragrunt.hcl", "HCL"],
    ["prod.tfvars", "HCL"],
    ["a.tftest", "HCL"],
    ["user-data.tftpl", "HCL"],
    ["prod.tfvars.json", "JSON"],
    ["package.json", "JSON"],
    ["README.md", "Markdown"],
    ["notes.markdown", "Markdown"],
    ["ci.yml", "YAML"],
    ["ci.YAML", "YAML"],
    ["index.js", "JavaScript"],
    ["a.mjs", "JavaScript"],
    ["a.cjs", "JavaScript"],
    ["a.jsx", "JavaScript"],
    ["a.ts", "TypeScript"],
    ["a.mts", "TypeScript"],
    ["a.cts", "TypeScript"],
    ["a.tsx", "TypeScript"],
    ["handler.py", "Python"],
    ["deploy.sh", "Shell"],
    ["a.bash", "Shell"],
    ["a.zsh", "Shell"],
    [".envrc", "Shell"],
    ["pyproject.toml", "TOML"],
    [".env", "Properties"],
    ["a.ini", "Properties"],
    ["a.cfg", "Properties"],
    ["a.conf", "Properties"],
    ["a.properties", "Properties"],
    ["Dockerfile", "Dockerfile"],
    ["app.dockerfile", "Dockerfile"],
    ["main.go", "Go"],
  ])("%s → %s", (name, label) => {
    expect(languageFor(name)?.label).toBe(label);
  });

  it("is plain text for anything else", () => {
    expect(languageFor("LICENSE")).toBeNull();
    expect(languageFor("notes.txt")).toBeNull();
  });
});

describe("createCodeEditor", () => {
  const make = (fileName = "main.tf", doc = 'resource "a" "b" {}\n') => {
    const parent = document.createElement("div");
    document.body.replaceChildren(parent);
    const o = { fileName, doc, onChange: vi.fn(), onBlur: vi.fn() };
    return { parent, o, editor: createCodeEditor(parent, o) };
  };

  it("shows the document with highlighting for its language", () => {
    const { parent, editor } = make();
    expect(editor.getValue()).toBe('resource "a" "b" {}\n');
    expect(parent.querySelector(".cm-editor")).not.toBeNull();
    const classes = new Set<string>();
    highlightTree(syntaxTree(editor.view.state), { style: (tags) => (tags.length ? "x" : null) }, (_f, _t, c) => classes.add(c));
    expect(classes.size).toBeGreaterThan(0);
    editor.destroy();
    expect(parent.querySelector(".cm-editor")).toBeNull();
  });

  it("works for plain text", () => {
    const { editor } = make("LICENSE", "text");
    expect(editor.getValue()).toBe("text");
    expect(syntaxTree(editor.view.state).length).toBe(0);
    editor.destroy();
  });

  it("reports changes and blur, and indents with Tab", () => {
    const { editor, o } = make();
    editor.view.dispatch({ selection: { anchor: 0 } });
    expect(o.onChange).not.toHaveBeenCalled();
    editor.view.dispatch({ changes: { from: 0, insert: "# hi\n" } });
    expect(o.onChange).toHaveBeenCalledTimes(1);
    expect(editor.getValue().startsWith("# hi\n")).toBe(true);

    // Tab indents rather than leaving the editor.
    const tab = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    editor.view.contentDOM.dispatchEvent(tab);
    expect(tab.defaultPrevented).toBe(true);

    editor.focus();
    expect(document.activeElement).toBe(editor.view.contentDOM);
    editor.view.contentDOM.dispatchEvent(new FocusEvent("blur"));
    expect(o.onBlur).toHaveBeenCalled();
    editor.destroy();
  });
});

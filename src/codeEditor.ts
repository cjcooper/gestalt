// CodeMirror set up for the file editor: language by file name, colours from
// the app's palette (so light/dark mode follow the system), and hooks for
// saving. (⌘S is handled by the editor window around it.)

import { basicSetup } from "codemirror";
import { indentWithTab } from "@codemirror/commands";
import { HighlightStyle, StreamLanguage, syntaxHighlighting, type StreamParser } from "@codemirror/language";
import { EditorState, type Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { json } from "@codemirror/lang-json";
import { markdown } from "@codemirror/lang-markdown";
import { yaml } from "@codemirror/lang-yaml";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { shell } from "@codemirror/legacy-modes/mode/shell";
import { toml } from "@codemirror/legacy-modes/mode/toml";
import { properties } from "@codemirror/legacy-modes/mode/properties";
import { dockerFile } from "@codemirror/legacy-modes/mode/dockerfile";
import { go } from "@codemirror/legacy-modes/mode/go";
import { hcl } from "./hcl";

const legacy = (parser: StreamParser<unknown>) => StreamLanguage.define(parser);

/** The language for a file, by name, and a short label for it; null for plain text. */
export function languageFor(fileName: string): { label: string; extension: Extension } | null {
  const n = fileName.toLowerCase();
  const ext = n.includes(".") ? n.slice(n.lastIndexOf(".") + 1) : "";
  if (n.endsWith(".tfvars.json") || ext === "json") return { label: "JSON", extension: json() };
  if (["tf", "tofu", "hcl", "tfvars", "tftest", "tftpl"].includes(ext)) return { label: "HCL", extension: legacy(hcl as StreamParser<unknown>) };
  if (ext === "md" || ext === "markdown") return { label: "Markdown", extension: markdown() };
  if (ext === "yaml" || ext === "yml") return { label: "YAML", extension: yaml() };
  if (["js", "mjs", "cjs", "jsx"].includes(ext)) return { label: "JavaScript", extension: javascript({ jsx: true }) };
  if (["ts", "mts", "cts", "tsx"].includes(ext)) return { label: "TypeScript", extension: javascript({ typescript: true, jsx: ext === "tsx" }) };
  if (ext === "py") return { label: "Python", extension: python() };
  if (["sh", "bash", "zsh", "envrc"].includes(ext) || n === ".envrc") return { label: "Shell", extension: legacy(shell) };
  if (ext === "toml") return { label: "TOML", extension: legacy(toml) };
  if (["env", "ini", "cfg", "conf", "properties"].includes(ext)) return { label: "Properties", extension: legacy(properties) };
  if (n === "dockerfile" || ext === "dockerfile") return { label: "Dockerfile", extension: legacy(dockerFile) };
  if (ext === "go") return { label: "Go", extension: legacy(go) };
  return null;
}

/** Token colours from the app's ANSI palette (styles.css), so they track light/dark mode. */
const highlight = HighlightStyle.define([
  { tag: [t.keyword, t.controlKeyword, t.definitionKeyword, t.moduleKeyword, t.operatorKeyword], color: "var(--ansi-5)" },
  { tag: [t.string, t.special(t.string), t.regexp], color: "var(--ansi-2)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--ansi-3)" },
  { tag: [t.comment, t.lineComment, t.blockComment, t.docComment], color: "var(--muted)", fontStyle: "italic" },
  { tag: [t.propertyName, t.attributeName], color: "var(--ansi-4)" },
  { tag: [t.function(t.variableName), t.function(t.propertyName)], color: "var(--ansi-6)" },
  { tag: [t.typeName, t.className, t.namespace], color: "var(--ansi-6)" },
  { tag: [t.meta, t.processingInstruction], color: "var(--ansi-1)" },
  { tag: [t.heading], color: "var(--accent)", fontWeight: "bold" },
  { tag: t.strong, fontWeight: "bold" },
  { tag: t.emphasis, fontStyle: "italic" },
  { tag: t.link, color: "var(--ansi-4)", textDecoration: "underline" },
  { tag: t.invalid, color: "var(--bad)" },
]);

const theme = EditorView.theme({
  "&": { height: "100%", backgroundColor: "var(--output-bg)", color: "var(--fg)" },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": { fontFamily: 'ui-monospace, "SF Mono", Menlo, Consolas, monospace', fontSize: "12.5px", lineHeight: "1.5" },
  ".cm-gutters": { backgroundColor: "var(--bg-alt)", color: "var(--muted)", borderRight: "1px solid var(--border)" },
  ".cm-activeLine": { backgroundColor: "color-mix(in srgb, var(--accent) 6%, transparent)" },
  ".cm-activeLineGutter": { backgroundColor: "color-mix(in srgb, var(--accent) 12%, transparent)", color: "var(--fg)" },
  ".cm-cursor": { borderLeftColor: "var(--fg)" },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection": {
    backgroundColor: "var(--highlight)",
  },
  ".cm-panels": { backgroundColor: "var(--bg-alt)", color: "var(--fg)" },
  ".cm-tooltip": { backgroundColor: "var(--bg)", border: "1px solid var(--border)" },
});

export interface CodeEditor {
  view: EditorView;
  getValue(): string;
  focus(): void;
  destroy(): void;
}

export interface CodeEditorOptions {
  fileName: string;
  doc: string;
  /** The text changed. */
  onChange: () => void;
  /** The editor lost focus. */
  onBlur: () => void;
}

export function createCodeEditor(parent: HTMLElement, o: CodeEditorOptions): CodeEditor {
  const lang = languageFor(o.fileName);
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: o.doc,
      extensions: [
        keymap.of([indentWithTab]),
        basicSetup,
        theme,
        syntaxHighlighting(highlight),
        lang ? lang.extension : [],
        EditorView.updateListener.of((u) => {
          if (u.docChanged) o.onChange();
        }),
        EditorView.domEventHandlers({
          blur: () => {
            o.onBlur();
          },
        }),
      ],
    }),
  });
  return {
    view,
    getValue: () => view.state.doc.toString(),
    focus: () => view.focus(),
    destroy: () => view.destroy(),
  };
}

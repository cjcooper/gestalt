// CodeMirror set up for the file editor: language by file name, colours from
// the app's palette (so light/dark mode follow the system), and hooks for
// saving. (⌘S is handled by the editor window around it.)

import { basicSetup } from "codemirror";
import { indentWithTab } from "@codemirror/commands";
import { HighlightStyle, StreamLanguage, syntaxHighlighting, type StreamParser } from "@codemirror/language";
import { Compartment, EditorState, type Extension } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";
import { tags as t } from "@lezer/highlight";
import { hcl } from "./hcl";

// Language support is loaded on demand: the grammars are most of CodeMirror's
// weight, and an editor window only ever needs one of them.

const legacy = (parser: StreamParser<unknown>) => StreamLanguage.define(parser);

export interface Language {
  /** A short label for the status bar. */
  label: string;
  /** Load the CodeMirror extension that highlights it. */
  load: () => Promise<Extension>;
}

/** The language for a file, by name; null for plain text. */
export function languageFor(fileName: string): Language | null {
  const n = fileName.toLowerCase();
  const ext = n.includes(".") ? n.slice(n.lastIndexOf(".") + 1) : "";
  const lang = (label: string, load: () => Promise<Extension>): Language => ({ label, load });
  if (n.endsWith(".tfvars.json") || ext === "json") return lang("JSON", async () => (await import("@codemirror/lang-json")).json());
  if (["tf", "tofu", "hcl", "tfvars", "tftest", "tftpl"].includes(ext)) return lang("HCL", async () => legacy(hcl as StreamParser<unknown>));
  if (ext === "md" || ext === "markdown") return lang("Markdown", async () => (await import("@codemirror/lang-markdown")).markdown());
  if (ext === "yaml" || ext === "yml") return lang("YAML", async () => (await import("@codemirror/lang-yaml")).yaml());
  if (["js", "mjs", "cjs", "jsx"].includes(ext))
    return lang("JavaScript", async () => (await import("@codemirror/lang-javascript")).javascript({ jsx: true }));
  if (["ts", "mts", "cts", "tsx"].includes(ext))
    return lang("TypeScript", async () => (await import("@codemirror/lang-javascript")).javascript({ typescript: true, jsx: ext === "tsx" }));
  if (ext === "py") return lang("Python", async () => (await import("@codemirror/lang-python")).python());
  if (["sh", "bash", "zsh", "envrc"].includes(ext) || n === ".envrc")
    return lang("Shell", async () => legacy((await import("@codemirror/legacy-modes/mode/shell")).shell));
  if (ext === "toml") return lang("TOML", async () => legacy((await import("@codemirror/legacy-modes/mode/toml")).toml));
  if (["env", "ini", "cfg", "conf", "properties"].includes(ext))
    return lang("Properties", async () => legacy((await import("@codemirror/legacy-modes/mode/properties")).properties));
  if (n === "dockerfile" || ext === "dockerfile")
    return lang("Dockerfile", async () => legacy((await import("@codemirror/legacy-modes/mode/dockerfile")).dockerFile));
  if (ext === "go") return lang("Go", async () => legacy((await import("@codemirror/legacy-modes/mode/go")).go));
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
  /** Settles once the file's language is loaded and applied (or failed to load). */
  ready: Promise<void>;
  getValue(): string;
  /** Select line `n` (1-based, clamped to the file) and scroll it to the middle. */
  gotoLine(n: number): void;
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
  const language = new Compartment();
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: o.doc,
      extensions: [
        keymap.of([indentWithTab]),
        basicSetup,
        theme,
        syntaxHighlighting(highlight),
        language.of([]),
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
  let destroyed = false;
  // Without its grammar the file still edits fine as plain text, so a failed
  // load is only logged.
  const ready = (languageFor(o.fileName)?.load() ?? Promise.resolve([]))
    .then((ext) => {
      if (!destroyed) view.dispatch({ effects: language.reconfigure(ext) });
    })
    .catch((e) => console.error(`couldn't load highlighting for ${o.fileName}:`, e));
  return {
    view,
    ready,
    getValue: () => view.state.doc.toString(),
    gotoLine: (n) => {
      const doc = view.state.doc;
      const line = doc.line(Math.min(Math.max(1, Math.floor(n)), doc.lines));
      view.dispatch({
        selection: { anchor: line.from, head: line.to },
        effects: EditorView.scrollIntoView(line.from, { y: "center" }),
      });
    },
    focus: () => view.focus(),
    destroy: () => {
      destroyed = true;
      view.destroy();
    },
  };
}

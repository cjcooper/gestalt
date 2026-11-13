// A small HCL / OpenTofu tokenizer for CodeMirror's StreamLanguage: enough to
// colour .tf, .tofu, .hcl and .tfvars files (blocks, attributes, strings with
// ${…} interpolation, heredocs, comments, numbers, functions).

import type { StreamParser, StringStream } from "@codemirror/language";

export interface HclState {
  /** Nested contexts: inside a "string", a heredoc, or a ${…} interpolation. */
  stack: ({ kind: "string" } | { kind: "heredoc"; end: string } | { kind: "interp"; depth: number })[];
  inBlockComment: boolean;
  /** Next line starts a heredoc ending at this word. */
  pendingHeredoc: string | null;
}

const BLOCKS = new Set([
  "resource",
  "data",
  "variable",
  "output",
  "module",
  "provider",
  "terraform",
  "locals",
  "moved",
  "import",
  "removed",
  "check",
  "backend",
  "required_providers",
  "dynamic",
  "content",
  "lifecycle",
  "provisioner",
  "connection",
  "validation",
]);
const KEYWORDS = new Set(["for", "in", "if", "else", "endif", "endfor"]);
const ATOMS = new Set(["true", "false", "null"]);

/** Inside "…" or a heredoc: text up to the next ${ or %{ or the end. */
function stringBody(stream: StringStream, state: HclState, quoted: boolean): string {
  if (stream.match("${") || stream.match("%{")) {
    state.stack.push({ kind: "interp", depth: 0 });
    return "meta";
  }
  if (quoted) {
    while (!stream.eol()) {
      if (stream.match(/^\\./)) continue;
      if (stream.peek() === '"') {
        stream.next();
        state.stack.pop();
        return "string";
      }
      if (stream.match("${", false) || stream.match("%{", false)) return "string";
      stream.next();
    }
    return "string";
  }
  // Heredoc content: the closing word on its own line ends it.
  const top = state.stack[state.stack.length - 1] as { kind: "heredoc"; end: string };
  if (stream.sol() && stream.match(new RegExp(`^\\s*${top.end}\\s*$`))) {
    state.stack.pop();
    return "string";
  }
  while (!stream.eol() && !stream.match("${", false) && !stream.match("%{", false)) stream.next();
  return "string";
}

function token(stream: StringStream, state: HclState): string | null {
  const top = state.stack[state.stack.length - 1];
  if (stream.sol() && state.pendingHeredoc) {
    state.stack.push({ kind: "heredoc", end: state.pendingHeredoc });
    state.pendingHeredoc = null;
    return stringBody(stream, state, false);
  }
  if (top?.kind === "string") return stringBody(stream, state, true);
  if (top?.kind === "heredoc") return stringBody(stream, state, false);

  if (state.inBlockComment) {
    if (stream.skipTo("*/")) {
      stream.match("*/");
      state.inBlockComment = false;
    } else stream.skipToEnd();
    return "comment";
  }
  if (stream.eatSpace()) return null;

  if (stream.match("#") || stream.match("//")) {
    stream.skipToEnd();
    return "comment";
  }
  if (stream.match("/*")) {
    state.inBlockComment = true;
    return "comment";
  }

  if (top?.kind === "interp") {
    if (stream.peek() === "{") top.depth++;
    else if (stream.peek() === "}") {
      if (top.depth === 0) {
        stream.next();
        state.stack.pop();
        return "meta";
      }
      top.depth--;
    }
  }

  if (stream.peek() === '"') {
    stream.next();
    state.stack.push({ kind: "string" });
    return "string";
  }
  const heredoc = stream.match(/^<<-?([A-Za-z_][\w-]*)/) as RegExpMatchArray | null;
  if (heredoc) {
    state.pendingHeredoc = heredoc[1];
    return "string";
  }
  if (stream.match(/^\d+(\.\d+)?([eE][+-]?\d+)?/)) return "number";

  const word = stream.match(/^[A-Za-z_][\w-]*/) as RegExpMatchArray | null;
  if (word) {
    const w = word[0];
    if (ATOMS.has(w)) return "atom";
    if (KEYWORDS.has(w)) return "keyword";
    if (stream.match(/^\s*\(/, false)) return "function";
    // `name =` is an attribute; `name {` / `name "label"` at line start is a block.
    if (stream.match(/^\s*=(?!=)/, false)) return "propertyName";
    if (BLOCKS.has(w) && stream.match(/^\s*(\{|")/, false)) return "keyword";
    if (stream.match(/^\s*\{/, false)) return "typeName";
    return "variableName";
  }

  if (stream.match(/^(==|!=|<=|>=|&&|\|\||=>|\.\.\.|[=!<>?:+\-*/%])/)) return "operator";
  stream.next();
  return "punctuation";
}

export const hcl: StreamParser<HclState> = {
  name: "hcl",
  startState: () => ({ stack: [], inBlockComment: false, pendingHeredoc: null }),
  copyState: (s) => ({ stack: s.stack.map((c) => ({ ...c })), inBlockComment: s.inBlockComment, pendingHeredoc: s.pendingHeredoc }),
  token,
  languageData: { commentTokens: { line: "#", block: { open: "/*", close: "*/" } } },
};

import { describe, expect, it } from "vitest";
import { StringStream } from "@codemirror/language";
import { hcl } from "./hcl";

/** Tokenize `src` as CodeMirror would: [text, style] per token, blanks skipped. */
function tokens(src: string): [string, string | null][] {
  const state = hcl.startState!(2);
  const out: [string, string | null][] = [];
  for (const line of src.split("\n")) {
    if (line === "") {
      continue; // StreamLanguage skips token() on blank lines
    }
    const stream = new StringStream(line, 2, 2);
    while (!stream.eol()) {
      const style = hcl.token(stream, state);
      if (stream.current().trim()) out.push([stream.current(), style]);
      stream.start = stream.pos;
    }
  }
  return out;
}

describe("hcl tokenizer", () => {
  it("colours blocks, labels, attributes and values", () => {
    expect(tokens('resource "aws_s3_bucket" "b" {\n  bucket = "x"\n  count  = 2\n  force = true\n}')).toEqual([
      ["resource", "keyword"],
      ['"', "string"],
      ['aws_s3_bucket"', "string"],
      ['"', "string"],
      ['b"', "string"],
      ["{", "punctuation"],
      ["bucket", "propertyName"],
      ["=", "operator"],
      ['"', "string"],
      ['x"', "string"],
      ["count", "propertyName"],
      ["=", "operator"],
      ["2", "number"],
      ["force", "propertyName"],
      ["=", "operator"],
      ["true", "atom"],
      ["}", "punctuation"],
    ]);
  });

  it("tells nested blocks, functions, references and keywords apart", () => {
    expect(tokens("lifecycle {\nsettings {\na = merge(var.x, local.y[0]) == null ? 1.5e3 : -1\nb = [for k in xs : k if k != \"\"]")).toEqual([
      ["lifecycle", "keyword"],
      ["{", "punctuation"],
      ["settings", "typeName"],
      ["{", "punctuation"],
      ["a", "propertyName"],
      ["=", "operator"],
      ["merge", "function"],
      ["(", "punctuation"],
      ["var", "variableName"],
      [".", "punctuation"],
      ["x", "variableName"],
      [",", "punctuation"],
      ["local", "variableName"],
      [".", "punctuation"],
      ["y", "variableName"],
      ["[", "punctuation"],
      ["0", "number"],
      ["]", "punctuation"],
      [")", "punctuation"],
      ["==", "operator"],
      ["null", "atom"],
      ["?", "operator"],
      ["1.5e3", "number"],
      [":", "operator"],
      ["-", "operator"],
      ["1", "number"],
      ["b", "propertyName"],
      ["=", "operator"],
      ["[", "punctuation"],
      ["for", "keyword"],
      ["k", "variableName"],
      ["in", "keyword"],
      ["xs", "variableName"],
      [":", "operator"],
      ["k", "variableName"],
      ["if", "keyword"],
      ["k", "variableName"],
      ["!=", "operator"],
      ['"', "string"],
      ['"', "string"],
      ["]", "punctuation"],
    ]);
    // A block keyword not opening a block is just a name.
    expect(tokens("data.x")[0]).toEqual(["data", "variableName"]);
  });

  it("handles comments of every kind", () => {
    expect(tokens('# a\n// b\nx = 1 # c\n/* d\n e */ y = 2\n/* f */')).toEqual([
      ["# a", "comment"],
      ["// b", "comment"],
      ["x", "propertyName"],
      ["=", "operator"],
      ["1", "number"],
      ["# c", "comment"],
      ["/*", "comment"],
      [" d", "comment"],
      [" e */", "comment"],
      ["y", "propertyName"],
      ["=", "operator"],
      ["2", "number"],
      ["/*", "comment"],
      [" f */", "comment"],
    ]);
  });

  it("highlights interpolation and escapes in strings", () => {
    expect(tokens('name = "a-${var.env}-\\"${lookup(m, "k", {})}\\" %{ if x }y%{ endif }"')).toEqual([
      ["name", "propertyName"],
      ["=", "operator"],
      ['"', "string"],
      ["a-", "string"],
      ["${", "meta"],
      ["var", "variableName"],
      [".", "punctuation"],
      ["env", "variableName"],
      ["}", "meta"],
      ['-\\"', "string"],
      ["${", "meta"],
      ["lookup", "function"],
      ["(", "punctuation"],
      ["m", "variableName"],
      [",", "punctuation"],
      ['"', "string"],
      ['k"', "string"],
      [",", "punctuation"],
      ["{", "punctuation"],
      ["}", "punctuation"],
      [")", "punctuation"],
      ["}", "meta"],
      ['\\" ', "string"],
      ["%{", "meta"],
      ["if", "keyword"],
      ["x", "variableName"],
      ["}", "meta"],
      ["y", "string"],
      ["%{", "meta"],
      ["endif", "keyword"],
      ["}", "meta"],
      ['"', "string"],
    ]);
  });

  it("continues an unterminated string onto the next line", () => {
    expect(tokens('a = "x\ny" b')).toEqual([
      ["a", "propertyName"],
      ["=", "operator"],
      ['"', "string"],
      ["x", "string"],
      ['y"', "string"],
      ["b", "variableName"],
    ]);
  });

  it("treats heredocs as strings with interpolation until the end marker", () => {
    expect(tokens("policy = <<-EOT\n  {\"a\": \"${var.b}\"}\n\n  EOT\nx = 1")).toEqual([
      ["policy", "propertyName"],
      ["=", "operator"],
      ["<<-EOT", "string"],
      ['  {"a": "', "string"],
      ["${", "meta"],
      ["var", "variableName"],
      [".", "punctuation"],
      ["b", "variableName"],
      ["}", "meta"],
      ['"}', "string"],
      ["  EOT", "string"],
      ["x", "propertyName"],
      ["=", "operator"],
      ["1", "number"],
    ]);
    // Empty heredoc: the marker is the first line.
    expect(tokens("a = <<EOF\nEOF\nb = 1").map((t) => t[1])).toEqual(["propertyName", "operator", "string", "string", "propertyName", "operator", "number"]);
  });

  it("copies state deeply", () => {
    const s = hcl.startState!(2);
    s.stack.push({ kind: "interp", depth: 1 });
    s.pendingHeredoc = "EOF";
    s.inBlockComment = true;
    const c = hcl.copyState!(s);
    expect(c).toEqual(s);
    (c.stack[0] as { depth: number }).depth = 5;
    expect((s.stack[0] as { depth: number }).depth).toBe(1);
  });
});

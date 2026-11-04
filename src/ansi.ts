// Rendering tofu's ANSI-coloured output as styled spans.
//
// Only SGR (`ESC [ … m`) is interpreted; any other escape sequence (cursor
// movement, OSC titles/links) is dropped. Styling carries across lines, as in
// a terminal, so each output stream keeps its own `AnsiState`.

export interface AnsiState {
  bold: boolean;
  dim: boolean;
  italic: boolean;
  underline: boolean;
  inverse: boolean;
  /** 0–15 → themed palette class; 16–255 / truecolor → `rgb(...)` string. */
  fg: number | string | null;
  bg: number | string | null;
}

export const newAnsiState = (): AnsiState => ({
  bold: false,
  dim: false,
  italic: false,
  underline: false,
  inverse: false,
  fg: null,
  bg: null,
});

// CSI (ESC [ params final), OSC (ESC ] … BEL | ESC \), or a lone two-byte escape.
const ESCAPE = /\x1b\[([0-9;:?]*)([@-~])|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-_]/g;

export function stripAnsi(s: string): string {
  return s.replace(ESCAPE, "");
}

/** xterm 256-colour palette, entries 16–255. */
function color256(n: number): string {
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return `rgb(${v},${v},${v})`;
  }
  const i = n - 16;
  const c = (x: number) => (x === 0 ? 0 : 55 + x * 40);
  return `rgb(${c(Math.floor(i / 36))},${c(Math.floor(i / 6) % 6)},${c(i % 6)})`;
}

/** Parse an extended colour (`5;n` or `2;r;g;b`) starting at codes[i]; returns [colour, next index]. */
function extended(codes: number[], i: number): [number | string | null, number] {
  if (codes[i] === 5) {
    const n = codes[i + 1];
    if (n == null || n > 255) return [null, i + 2];
    return [n < 16 ? n : color256(n), i + 2];
  }
  if (codes[i] === 2) {
    const [r, g, b] = codes.slice(i + 1, i + 4).map((x) => Math.min(x ?? 0, 255));
    return [`rgb(${r},${g},${b})`, i + 4];
  }
  return [null, codes.length];
}

function applySgr(state: AnsiState, params: string) {
  const codes = params === "" ? [0] : params.split(/[;:]/).map((p) => (p === "" ? 0 : Number(p)));
  for (let i = 0; i < codes.length; i++) {
    const c = codes[i];
    if (c === 0) Object.assign(state, newAnsiState());
    else if (c === 1) state.bold = true;
    else if (c === 2) state.dim = true;
    else if (c === 3) state.italic = true;
    else if (c === 4) state.underline = true;
    else if (c === 7) state.inverse = true;
    else if (c === 22) state.bold = state.dim = false;
    else if (c === 23) state.italic = false;
    else if (c === 24) state.underline = false;
    else if (c === 27) state.inverse = false;
    else if (c >= 30 && c <= 37) state.fg = c - 30;
    else if (c === 39) state.fg = null;
    else if (c >= 40 && c <= 47) state.bg = c - 40;
    else if (c === 49) state.bg = null;
    else if (c >= 90 && c <= 97) state.fg = c - 90 + 8;
    else if (c >= 100 && c <= 107) state.bg = c - 100 + 8;
    else if (c === 38 || c === 48) {
      const [color, next] = extended(codes, i + 1);
      if (c === 38) state.fg = color;
      else state.bg = color;
      i = next - 1;
    }
  }
}

const isPlain = (s: AnsiState) =>
  !s.bold && !s.dim && !s.italic && !s.underline && !s.inverse && s.fg == null && s.bg == null;

function styled(text: string, s: AnsiState): Node {
  if (isPlain(s)) return document.createTextNode(text);
  const span = document.createElement("span");
  span.textContent = text;
  const cls: string[] = [];
  if (s.bold) cls.push("ansi-bold");
  if (s.dim) cls.push("ansi-dim");
  if (s.italic) cls.push("ansi-italic");
  if (s.underline) cls.push("ansi-underline");
  let [fg, bg] = s.inverse ? [s.bg, s.fg] : [s.fg, s.bg];
  if (s.inverse) {
    // Swapping a default colour means using the pane's own fg/bg.
    fg ??= "var(--output-bg)";
    bg ??= "var(--fg)";
  }
  if (typeof fg === "number") cls.push(`ansi-fg-${fg}`);
  else if (fg) span.style.color = fg;
  if (typeof bg === "number") cls.push(`ansi-bg-${bg}`);
  else if (bg) span.style.backgroundColor = bg;
  span.className = cls.join(" ");
  return span;
}

/** Render one line of output, updating `state` with the styling it leaves set. */
export function renderAnsi(line: string, state: AnsiState): DocumentFragment {
  const frag = document.createDocumentFragment();
  let last = 0;
  for (const m of line.matchAll(ESCAPE)) {
    if (m.index > last) frag.appendChild(styled(line.slice(last, m.index), state));
    if (m[2] === "m") applySgr(state, m[1]);
    last = m.index + m[0].length;
  }
  if (last < line.length) frag.appendChild(styled(line.slice(last), state));
  return frag;
}

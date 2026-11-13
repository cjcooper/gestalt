#!/usr/bin/env node
// Builds Gestalt's third-party license inventory from what actually ships:
// Rust crates linked into the app (cargo-about, see about.toml), production
// npm packages bundled into the UI (pnpm licenses), and adapted content.
//
// Writes:
//   public/third-party-licenses.json   (the About window)
//   THIRD_PARTY_NOTICES.md             (the same list, for the repo)
//
//   node scripts/licenses.mjs           regenerate
//   node scripts/licenses.mjs --check   fail if the files are stale
//
// Needs cargo-about: `cargo install cargo-about`.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const jsonOut = join(root, "public/third-party-licenses.json");
const mdOut = join(root, "THIRD_PARTY_NOTICES.md");
const check = process.argv.includes("--check");
const windows = process.platform === "win32";

const WORKSPACE = new Set(["gestalt"]);

/** Identical license texts are stored once and referenced by id. */
const texts = new Map();
function textId(raw) {
  const text = raw.replace(/\r\n/g, "\n").trim() + "\n";
  const id = createHash("sha256").update(text).digest("hex").slice(0, 10);
  texts.set(id, text);
  return id;
}

/** name@version → package entry; a crate under "MIT AND Unicode-3.0" shows up once. */
const packages = new Map();
function add(entry, licenseId, textIds) {
  const key = `${entry.ecosystem}:${entry.name}@${entry.version}`;
  const p = packages.get(key) ?? { ...entry, licenses: [], texts: [] };
  if (!p.licenses.includes(licenseId)) p.licenses.push(licenseId);
  for (const t of textIds) if (!p.texts.includes(t)) p.texts.push(t);
  packages.set(key, p);
}

// ---- Rust -----------------------------------------------------------------
const about = JSON.parse(
  execFileSync(
    "cargo",
    [
      "about",
      "generate",
      "--format",
      "json",
      "--locked",
      "--manifest-path",
      join(root, "src-tauri/Cargo.toml"),
      "-c",
      join(root, "about.toml"),
    ],
    { cwd: root, encoding: "utf8", maxBuffer: 1 << 28, stdio: ["ignore", "pipe", "inherit"] },
  ),
);
for (const lic of about.licenses) {
  for (const { crate } of lic.used_by) {
    if (WORKSPACE.has(crate.name)) continue;
    add(
      {
        ecosystem: "cargo",
        name: crate.name,
        version: crate.version,
        declared: crate.license ?? lic.id,
        url: crate.repository ?? crate.homepage ?? `https://crates.io/crates/${crate.name}`,
      },
      lic.id,
      [textId(lic.text)],
    );
  }
}

// ---- npm ------------------------------------------------------------------
const npm = JSON.parse(
  execFileSync(windows ? "pnpm.cmd" : "pnpm", ["licenses", "list", "--prod", "--json"], {
    cwd: root,
    encoding: "utf8",
    shell: windows,
  }),
);
const missing = [];
for (const [license, list] of Object.entries(npm)) {
  for (const pkg of list) {
    pkg.versions.forEach((version, i) => {
      const dir = pkg.paths[i] ?? pkg.paths[0];
      const files = readdirSync(dir).filter((f) => /^(licen[cs]e|copying|notice)/i.test(f)).sort();
      if (files.length === 0) missing.push(`${pkg.name}@${version}`);
      add(
        {
          ecosystem: "npm",
          name: pkg.name,
          version,
          declared: license,
          url: pkg.homepage ?? `https://www.npmjs.com/package/${pkg.name}`,
        },
        license,
        files.map((f) => textId(readFileSync(join(dir, f), "utf8"))),
      );
    });
  }
}

// ---- Content --------------------------------------------------------------
// Adapted content that isn't a package dependency. Keep the license text
// exactly as the project publishes it.
add(
  {
    ecosystem: "content",
    name: "Devicon",
    version: "2.16.0",
    declared: "MIT",
    url: "https://devicon.dev",
    note: "The AWS and Azure marks in src/icons.ts are Devicon's single-colour icons. The Google Cloud mark is from Simple Icons (CC0 1.0, no notice required).",
  },
  "MIT",
  [
    textId(`The MIT License (MIT)

Copyright (c) 2015 konpa

Permission is hereby granted, free of charge, to any person obtaining a copy of
this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software is furnished to do so,
subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS
FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR
COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
`),
  ],
);
add(
  {
    ecosystem: "content",
    name: "Lucide",
    version: "—",
    declared: "ISC",
    url: "https://lucide.dev",
    note: "The UI's inline icons (src/icons.ts) follow Lucide's shapes. Some Lucide icons derive from Feather (MIT).",
  },
  "ISC",
  [
    textId(`ISC License

Copyright (c) 2026 Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

---

Icons derived from the Feather project (https://feathericons.com) are under:

The MIT License (MIT)

Copyright (c) 2013-present Cole Bemis

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.`),
  ],
);

// ---- Output ---------------------------------------------------------------
const list = [...packages.values()].sort(
  (a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version, undefined, { numeric: true }),
);
const summary = {};
for (const p of list) for (const l of p.licenses) summary[l] = (summary[l] ?? 0) + 1;
const used = new Set(list.flatMap((p) => p.texts));

// Gestalt's own license (LICENSE).
const licenseText = readFileSync(join(root, "LICENSE"), "utf8");
const inventory = {
  generated_by: "scripts/licenses.mjs",
  gestalt: {
    name: "Gestalt",
    license: "MIT",
    copyright: licenseText.match(/^Copyright .*$/m)?.[0] ?? "",
    text: licenseText,
  },
  summary: Object.fromEntries(Object.entries(summary).sort((a, b) => b[1] - a[1])),
  packages: list,
  texts: Object.fromEntries([...texts].filter(([id]) => used.has(id)).sort()),
};
const json = JSON.stringify(inventory) + "\n";

const md = [];
md.push("# Third-party notices", "");
md.push(
  "Gestalt is MIT-licensed (see `LICENSE`). It includes the third-party software and content listed",
  "below, each under its own license. The full license texts follow the list. Where a package offers",
  "a choice of licenses, the one Gestalt uses is listed first. The source for MPL-2.0 packages is",
  "available from the URL given for each one.",
  "",
  "This file is generated by `scripts/licenses.mjs`. Don't edit it by hand.",
  "",
  "| License | Packages |",
  "|---|---|",
  ...Object.entries(inventory.summary).map(([l, n]) => `| ${l} | ${n} |`),
  "",
  "## Packages",
  "",
  "| Package | Version | Source | License | Text |",
  "|---|---|---|---|---|",
);
for (const p of list) {
  const refs = p.texts.map((t) => `[${t}](#license-${t})`).join(", ") || "—";
  md.push(`| ${p.name} | ${p.version} | ${p.url} | ${p.licenses.join(", ")} | ${refs} |`);
}
md.push("", "## License texts", "");
for (const [id, text] of Object.entries(inventory.texts)) {
  md.push(`<a id="license-${id}"></a>`, `### ${id}`, "", "```text", text.trimEnd(), "```", "");
}
const markdown = md.join("\n");

if (check) {
  const stale = [
    [jsonOut, json],
    [mdOut, markdown],
  ].filter(([path, want]) => {
    try {
      return readFileSync(path, "utf8") !== want;
    } catch {
      return true;
    }
  });
  if (stale.length) {
    console.error(`stale: ${stale.map(([p]) => p).join(", ")}. Run: node scripts/licenses.mjs`);
    process.exit(1);
  }
} else {
  mkdirSync(dirname(jsonOut), { recursive: true });
  writeFileSync(jsonOut, json);
  writeFileSync(mdOut, markdown);
}
console.error(
  `${list.length} packages, ${Object.keys(inventory.texts).length} distinct license texts` +
    (missing.length ? `; no license file shipped by: ${missing.join(", ")}` : ""),
);

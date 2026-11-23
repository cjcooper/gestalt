import { describe, expect, it } from "vitest";
import { completePath, matchPaths, normalizeQuery, type PathItem } from "./pathMatch";

const items: PathItem[] = [
  "envs/",
  "modules/",
  "README.md",
  "envs/dev/",
  "envs/prod/",
  "envs/common.tfvars",
  "envs/dev/main.tf",
  "envs/prod/main.tf",
  "envs/prod/prod.tfvars",
  "modules/network/",
  "modules/network/main.tf",
  "modules/network/variables.tf",
].map((p) => ({ rel: p.replace(/\/$/, ""), path: `/repo/${p.replace(/\/$/, "")}`, isDir: p.endsWith("/") }));

const texts = (query: string, limit?: number) => matchPaths(items, query, limit).map((m) => m.text);
const hits = (query: string, text: string) => matchPaths(items, query).find((m) => m.text === text)!.hits;

describe("normalizeQuery", () => {
  it("reads what's typed as a relative path", () => {
    expect(normalizeQuery("  ./envs/prod ")).toBe("envs/prod");
    expect(normalizeQuery("/envs")).toBe("envs");
    expect(normalizeQuery("envs\\prod\\")).toBe("envs/prod/");
    expect(normalizeQuery("   ")).toBe("");
  });
});

describe("matchPaths", () => {
  it("lists a typed folder's contents first, nearest first, folders before files", () => {
    expect(texts("envs/")).toEqual([
      "envs/",
      "envs/dev/",
      "envs/prod/",
      "envs/common.tfvars",
      "envs/dev/main.tf",
      "envs/prod/main.tf",
      "envs/prod/prod.tfvars",
    ]);
    expect(texts("envs/pr")).toEqual(["envs/prod/", "envs/prod/main.tf", "envs/prod/prod.tfvars"]);
    expect(texts("./ENVS/PROD/M")).toEqual(["envs/prod/main.tf"]);
    expect(hits("envs/pr", "envs/prod/main.tf")).toEqual([0, 1, 2, 3, 4, 5, 6]);
  });

  it("ranks by name: exact, then starting with, then containing, then anywhere in the path", () => {
    expect(texts("main.tf")).toEqual(["envs/dev/main.tf", "envs/prod/main.tf", "modules/network/main.tf"]);
    expect(texts("prod")).toEqual(["envs/prod/", "envs/prod/prod.tfvars", "envs/prod/main.tf"]);
    expect(texts("vars")).toEqual(["envs/common.tfvars", "envs/prod/prod.tfvars", "modules/network/variables.tf"]);
    // In the name: the name's letters are the hits.
    expect(hits("prod", "envs/prod/prod.tfvars")).toEqual([10, 11, 12, 13]);
    expect(hits("work", "modules/network/")).toEqual([11, 12, 13, 14]);
    // Only in the folders: the path's letters.
    expect(hits("prod", "envs/prod/main.tf")).toEqual([5, 6, 7, 8]);
    // In the path beats letters in order (envs/common.tfvars has s, /, n).
    expect(texts("s/n")).toEqual([
      "modules/network/",
      "modules/network/main.tf",
      "modules/network/variables.tf",
      "envs/common.tfvars",
      "envs/dev/main.tf",
      "envs/prod/main.tf",
    ]);
  });

  it("falls back to letters in order, tightest first; spaces separate parts", () => {
    expect(texts("eprm")).toEqual(["envs/prod/main.tf"]);
    expect(texts("edm")).toEqual(["README.md", "envs/dev/main.tf", "envs/prod/main.tf"]);
    expect(hits("eprm", "envs/prod/main.tf")).toEqual([0, 5, 6, 10]);
    expect(texts("prod main")).toEqual(["envs/prod/main.tf"]);
    expect(texts("zzz")).toEqual([]);
  });

  it("matches nothing for nothing, and stops at the limit", () => {
    expect(texts("  ")).toEqual([]);
    expect(texts("e", 2)).toEqual(["envs/", "envs/dev/"]);
  });
});

describe("completePath", () => {
  it("completes to the end of the next folder, or the whole file", () => {
    expect(completePath("en", "envs/prod/main.tf")).toBe("envs/");
    expect(completePath("envs/", "envs/prod/main.tf")).toBe("envs/prod/");
    expect(completePath("./ENVS/PR", "envs/prod/")).toBe("envs/prod/");
    expect(completePath("envs/prod/ma", "envs/prod/main.tf")).toBe("envs/prod/main.tf");
  });

  it("takes a match that doesn't continue what's typed whole", () => {
    expect(completePath("main", "envs/dev/main.tf")).toBe("envs/dev/main.tf");
  });
});

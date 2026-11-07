import { describe, expect, it } from "vitest";
import { fileIcon, fileKind, icons } from "./icons";

describe("icons", () => {
  it("are all 16px currentColor SVGs", () => {
    for (const [name, markup] of Object.entries(icons)) {
      const div = document.createElement("div");
      div.innerHTML = markup;
      const svg = div.querySelector("svg");
      expect(svg, name).not.toBeNull();
      expect(svg!.getAttribute("width")).toBe("16");
      expect(svg!.getAttribute("stroke")).toBe("currentColor");
      expect(svg!.getAttribute("aria-hidden")).toBe("true");
      expect(svg!.children.length, name).toBeGreaterThan(0);
    }
  });
});

describe("fileKind", () => {
  it.each([
    [".gestalt.env", "env"],
    [".GESTALT.ENV", "env"],
    ["main.tf", "tofu"],
    ["thing.tofu", "tofu"],
    ["terragrunt.hcl", "tofu"],
    ["prod.tfvars", "vars"],
    ["prod.tfvars.json", "vars"],
    ["README.md", "text"],
    ["notes.txt", "text"],
    ["data.json", "text"],
    ["a.yml", "text"],
    ["a.yaml", "text"],
    ["Cargo.toml", "text"],
    ["run.sh", "text"],
    ["rules.mk", "text"],
    ["Makefile", "text"],
    ["image.png", "other"],
    ["env", "other"],
  ] as const)("%s → %s", (name, kind) => {
    expect(fileKind(name)).toBe(kind);
  });
});

describe("fileIcon", () => {
  it("picks an icon per kind", () => {
    expect(fileIcon("tofu")).toBe(icons.fileCode);
    expect(fileIcon("vars")).toBe(icons.fileVars);
    expect(fileIcon("env")).toBe(icons.key);
    expect(fileIcon("text")).toBe(icons.fileText);
    expect(fileIcon("other")).toBe(icons.file);
  });
});

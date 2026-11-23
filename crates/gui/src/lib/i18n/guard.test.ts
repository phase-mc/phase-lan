// Guard: Russian text for the player lives only in the dictionary (`lib/i18n/ru/`). Any Cyrillic
// in the window's strings and markup outside the dictionary is an error. Comments may be written in any language.
//
// An exception for a single line is the `i18n-allow` marker in a comment on the same line
// (for example, a language's name in itself).

import { describe, expect, it } from "vitest";

const CYRILLIC = /[Ѐ-ӿ]/;
const ALLOW = "i18n-allow";

/** Window sources as text (Vite reads them itself, without Node types): the key is the path from `src/`. */
const SOURCES = import.meta.glob(["../../**/*.{ts,svelte,css}", "../../../index.html"], {
  query: "?raw",
  import: "default",
  eager: true,
}) as Record<string, string>;

/** A file without comments (`//`, block and HTML) — by lines, numbering is preserved. */
function withoutComments(source: string): string[] {
  const blank = (text: string) => text.replace(/[^\n]/g, " ");
  const stripped = source
    .replace(/<!--[\s\S]*?-->/g, blank)
    .replace(/\/\*[\s\S]*?\*\//g, (block) => (block.includes(ALLOW) ? block : blank(block)));
  return stripped.split("\n").map((line) => {
    const allowed = line.includes(ALLOW);
    // `//` at the start or after whitespace/code, but not in addresses like https://…
    const cut = line.replace(/(^|[^:\\])\/\/.*$/, (_, before: string) => before);
    return allowed ? "" : cut;
  });
}

/** Finds Russian text outside the dictionary. */
export function findHardcodedCyrillic(): string[] {
  const found: string[] = [];
  for (const [path, source] of Object.entries(SOURCES)) {
    const rel = path.replace(/^(\.\.\/)+/, "");
    // The dictionary and tests altogether (they contain Russian expectations).
    if (/(^|\/)ru\/[^/]+\.ts$/.test(rel) || /\.test\.ts$/.test(rel)) continue;
    withoutComments(source).forEach((line, index) => {
      if (CYRILLIC.test(line)) found.push(`${rel}:${index + 1}: ${line.trim()}`);
    });
  }
  return found;
}

describe("сторож кириллицы", () => {
  it("сторож действительно читает экраны и компоненты", () => {
    const names = Object.keys(SOURCES).join("\n");
    for (const file of ["HostTab.svelte", "ProfileCapsule.svelte", "journal.ts", "App.svelte"]) {
      expect(names).toContain(file);
    }
  });

  it("в окне нет русских строк вне словаря", () => {
    expect(findHardcodedCyrillic()).toEqual([]);
  });

  it("сторож видит русский текст в строке и разметке, но не в комментарии", () => {
    const sample = [
      "// комментарий по-русски",
      'const a = "привет";',
      "/* блок",
      "   по-русски */",
      "<p>Текст</p>",
      "<!-- заметка -->",
      'const url = "https://example.com"; // хвост',
      'const ok = "Русский"; // i18n-allow: название языка',
    ].join("\n");
    const lines = withoutComments(sample).filter((line) => CYRILLIC.test(line));
    expect(lines.map((line) => line.trim())).toEqual(['const a = "привет";', "<p>Текст</p>"]);
  });
});

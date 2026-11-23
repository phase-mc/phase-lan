// Dictionary: completeness, identical placeholders, plurals, language selection order.

import { describe, expect, it } from "vitest";
import { en } from "./en";
import { ru } from "./ru";
import {
  fmtSize,
  language,
  LANGUAGES,
  parseLang,
  pluralCategory,
  resolveLanguage,
  t,
  translate,
  type Key,
} from "./index.svelte";
import { isPlural, type Msg } from "./types";

const DICTS = { ru, en } as const;
const keys = Object.keys(ru) as Key[];

const placeholders = (text: string): string[] =>
  [...text.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();

/** All strings of a message: for a plural — all its forms. */
const texts = (msg: Msg): string[] => (isPlural(msg) ? Object.values(msg) : [msg]);

describe("словарь", () => {
  it("в каждом языке есть все ключи русского, и лишних нет", () => {
    for (const { code, dict } of LANGUAGES) {
      expect(Object.keys(dict).sort(), code).toEqual([...keys].sort());
    }
  });

  it("подстановки {имя} одинаковы во всех языках и во всех формах склонения", () => {
    for (const key of keys) {
      const reference = new Set(texts(ru[key]).flatMap(placeholders));
      for (const { code, dict } of LANGUAGES) {
        for (const text of texts(dict[key])) {
          const names = new Set(placeholders(text));
          // A plural form may not repeat {n}: "One mod" — but the other names are the same.
          const missing = [...names].filter((name) => !reference.has(name));
          expect(missing, `${code}: ${key}`).toEqual([]);
        }
        const used = new Set(texts(dict[key]).flatMap(placeholders));
        const unused = [...reference].filter((name) => !used.has(name));
        expect(unused, `${code}: ${key} не использует {${unused.join("}, {")}}`).toEqual([]);
      }
    }
  });

  it("склонения: есть формы для всех категорий языка и запасная other", () => {
    for (const { code, dict } of LANGUAGES) {
      const needed = new Intl.PluralRules(code).resolvedOptions().pluralCategories;
      for (const key of keys) {
        const msg = dict[key] as Msg;
        if (!isPlural(msg)) continue;
        expect(typeof msg.other, `${code}: ${key}.other`).toBe("string");
        for (const category of needed) {
          expect(msg[category] ?? msg.other, `${code}: ${key}.${category}`).toBeTruthy();
        }
        // In Russian, integers take one/few/many — they must not be left to the fallback form.
        if (code === "ru") {
          for (const category of ["one", "few", "many"] as const) {
            expect(msg[category], `ru: ${key}.${category}`).toBeTruthy();
          }
        }
        if (code === "en") expect(msg.one, `en: ${key}.one`).toBeTruthy();
      }
    }
  });

  it("в английском словаре нет кириллицы, в русском нет «билет / endpoint / узел» для игрока", () => {
    const cyrillic = /[Ѐ-ӿ]/;
    for (const key of keys) {
      for (const text of texts(en[key])) expect(text, `en: ${key}`).not.toMatch(cyrillic);
    }
    // Project rule: the player never sees the banned words below (the Russian ones and the English ticket / node).
    const banned = /билет|endpoint|узел|узл[аыо]|\bticket|\bnode\b/i;
    for (const [code, dict] of Object.entries(DICTS)) {
      for (const key of keys) {
        for (const text of texts(dict[key] as Msg)) expect(text, `${code}: ${key}`).not.toMatch(banned);
      }
    }
  });

  it("ключи не пустые строки", () => {
    for (const { code, dict } of LANGUAGES) {
      for (const key of keys) {
        for (const text of texts(dict[key] as Msg)) expect(text.trim(), `${code}: ${key}`).not.toBe("");
      }
    }
  });
});

describe("склонения", () => {
  const mods = (lang: "ru" | "en", n: number) => translate(lang, "count.mods", { n });

  it("русский: один / несколько / много", () => {
    expect([1, 2, 4, 5, 11, 12, 14, 21, 22, 25, 100, 101].map((n) => mods("ru", n))).toEqual([
      "1 мод",
      "2 мода",
      "4 мода",
      "5 модов",
      "11 модов",
      "12 модов",
      "14 модов",
      "21 мод",
      "22 мода",
      "25 модов",
      "100 модов",
      "101 мод",
    ]);
  });

  it("английский: one / other", () => {
    expect([0, 1, 2, 5, 11, 21].map((n) => mods("en", n))).toEqual([
      "0 mods",
      "1 mod",
      "2 mods",
      "5 mods",
      "11 mods",
      "21 mods",
    ]);
  });

  it("категории Intl совпадают с ожидаемыми", () => {
    expect([1, 2, 5, 21].map((n) => pluralCategory("ru", n))).toEqual(["one", "few", "many", "one"]);
    expect([1, 2].map((n) => pluralCategory("en", n))).toEqual(["one", "other"]);
  });

  it("склонение по «n», а не по другому числу", () => {
    expect(translate("ru", "room.slotsUsed", { used: 3, n: 8 })).toBe("3 из 8 игроков");
    expect(translate("ru", "room.slotsUsed", { used: 3, n: 21 })).toBe("3 из 21 игрока");
    expect(translate("en", "room.slotsUsed", { used: 1, n: 1 })).toBe("1 of 1 player");
    expect(translate("en", "room.slotsUsed", { used: 3, n: 8 })).toBe("3 of 8 players");
  });
});

describe("подстановки и форматы", () => {
  it("подставляет параметры и оставляет неизвестные как есть", () => {
    expect(translate("en", "journal.disconnected", { who: "Steve" })).toBe("Player Steve disconnected.");
    expect(translate("ru", "journal.disconnected", { who: "Steve" })).toBe("Игрок Steve отключился.");
    expect(translate("en", "journal.disconnected")).toBe("Player {who} disconnected.");
  });

  it("t() читает текущий язык и переключается на лету", () => {
    expect(language.current).toBe("ru");
    expect(t("nav.journal")).toBe("Журнал");
    language.set("en");
    try {
      expect(t("nav.journal")).toBe("Log");
      expect(t("count.mods", { n: 2 })).toBe("2 mods");
      expect(fmtSize(12.34 * 1024 * 1024)).toBe("12.3 MB");
      expect(fmtSize(840 * 1024)).toBe("0.8 MB");
      expect(fmtSize(10 * 1024)).toBe("10 KB");
    } finally {
      language.set("ru");
    }
    expect(fmtSize(12.34 * 1024 * 1024).replace(/\s/g, " ")).toBe("12,3 МБ");
  });
});

describe("выбор языка", () => {
  it("сначала настройка, потом установщик, потом язык Windows", () => {
    expect(resolveLanguage("en", "ru", "ru-RU")).toBe("en");
    expect(resolveLanguage("ru", "en", "en-US")).toBe("ru");
    expect(resolveLanguage(null, "en", "ru-RU")).toBe("en");
    expect(resolveLanguage(null, "ru", "en-US")).toBe("ru");
    expect(resolveLanguage(null, null, "ru-RU")).toBe("ru");
    expect(resolveLanguage(null, null, "ru")).toBe("ru");
  });

  it("русский Windows — русский, любой другой — английский", () => {
    for (const tag of ["en-US", "de-DE", "uk-UA", "fr", "ja-JP", ""]) {
      expect(resolveLanguage(null, null, tag), tag).toBe("en");
    }
    expect(resolveLanguage(null, null, null)).toBe("en");
    expect(resolveLanguage(undefined, undefined, undefined)).toBe("en");
  });

  it("неизвестные значения пропускаются, а не ломают выбор", () => {
    expect(resolveLanguage("xx", "ru", "en-US")).toBe("ru");
    expect(resolveLanguage("", "zz", "ru-RU")).toBe("ru");
    expect(resolveLanguage("de", "fr", "ru-RU")).toBe("ru");
  });

  it("parseLang понимает регистр и региональные коды", () => {
    expect(parseLang("RU")).toBe("ru");
    expect(parseLang("en_US")).toBe("en");
    expect(parseLang("ru-RU")).toBe("ru");
    expect(parseLang("de")).toBeNull();
    expect(parseLang(null)).toBeNull();
  });

  it("у каждого языка есть название на нём самом", () => {
    expect(LANGUAGES.map((l) => l.native)).toEqual(["Русский", "English"]);
    expect(new Set(LANGUAGES.map((l) => l.code)).size).toBe(LANGUAGES.length);
  });
});

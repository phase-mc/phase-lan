// Window language: dictionary, substitutions, plurals and formats via `Intl`.
//
// `t("key", { params })` reads the reactive language, so in Svelte templates text
// is redrawn immediately when the language changes, without a restart. Keys and parameters are checked by
// TypeScript. A new language: a dictionary in `./<code>.ts` and a line in `LANGUAGES` — that's all.

import { en } from "./en";
import { ru } from "./ru";
import { isPlural, type Msg, type ParamsArg, type Translated } from "./types";

export type Key = keyof typeof ru;
export type Dictionary = Translated<typeof ru>;

interface LanguageInfo {
  /** Code: stored in settings, the installer registry and `room.json`. */
  code: string;
  /** Name in the language itself — that's how it appears in the menu. */
  native: string;
  /** Locale for `Intl`. */
  locale: string;
  dict: Dictionary;
}

/** Window languages, in display order. */
export const LANGUAGES = [
  { code: "ru", native: "Русский", locale: "ru-RU", dict: ru }, // i18n-allow: the language name in itself
  { code: "en", native: "English", locale: "en-US", dict: en },
] as const satisfies readonly LanguageInfo[];

export type Lang = (typeof LANGUAGES)[number]["code"];

export const DEFAULT_LANG: Lang = "ru";

export function isLang(value: unknown): value is Lang {
  return LANGUAGES.some((language) => language.code === value);
}

/** Code (`ru`, `EN`, `ru-RU`, `en_US`) → window language; unknown — `null`. */
export function parseLang(code: string | null | undefined): Lang | null {
  const primary = (code ?? "").trim().split(/[-_]/)[0]?.toLowerCase();
  return LANGUAGES.find((language) => language.code === primary)?.code ?? null;
}

/** Language choice in order: saved setting → installer choice → Windows language
 *  (Russian — Russian, any other — English). Unknown values are skipped. */
export function resolveLanguage(
  stored: string | null | undefined,
  installer: string | null | undefined,
  system: string | null | undefined,
): Lang {
  return (
    parseLang(stored) ?? parseLang(installer) ?? (system ? (parseLang(system) ?? "en") : "en")
  );
}

function dictionary(lang: Lang): Dictionary {
  return LANGUAGES.find((language) => language.code === lang)?.dict ?? ru;
}

/** Plural form for a number: the language's `one`/`few`/`many`/`other`. */
export function pluralCategory(lang: Lang, n: number): Intl.LDMLPluralRule {
  return new Intl.PluralRules(localeOf(lang)).select(n);
}

export function localeOf(lang: Lang): string {
  return LANGUAGES.find((language) => language.code === lang)?.locale ?? "en-US";
}

function pick(msg: Msg, lang: Lang, params?: Record<string, string | number>): string {
  if (!isPlural(msg)) return msg;
  const n = Number(params?.n ?? 0);
  return msg[pluralCategory(lang, n)] ?? msg.other;
}

function fill(text: string, params?: Record<string, string | number>): string {
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) =>
    name in params ? String(params[name]) : whole,
  );
}

/** Translation without reactivity: the language is given explicitly (tests, background code). */
export function translate(
  lang: Lang,
  key: Key,
  params?: Record<string, string | number>,
): string {
  const msg: Msg | undefined = dictionary(lang)[key] ?? ru[key];
  if (msg === undefined) return key;
  return fill(pick(msg, lang, params), params);
}

class Language {
  current = $state<Lang>(DEFAULT_LANG);

  set(lang: Lang): void {
    this.current = lang;
    try {
      document.documentElement.lang = lang;
    } catch {
      // No document (tests in Node) — the page language isn't needed.
    }
  }
}

export const language = new Language();

/** Text in the current language: `t("host.create")`, `t("mods.count", { n: 3 })`. */
export function t<K extends Key>(
  key: K,
  ...args: K extends Key ? ParamsArg<(typeof ru)[K]> : never
): string {
  return translate(language.current, key, args[0] as Record<string, string | number> | undefined);
}

// ---------------------------------------------------------------- formats (Intl)

const locale = () => localeOf(language.current);

/** A number by the language's rules: "12,345.6" in en, "12 345,6" in ru. */
export function fmtNumber(n: number, options?: Intl.NumberFormatOptions): string {
  return new Intl.NumberFormat(locale(), options).format(n);
}

/** Time of day: "2:05:09 PM" in en, "14:05:09" in ru. */
export function fmtTime(date: Date): string {
  return new Intl.DateTimeFormat(locale(), {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(date);
}

/** Size: "12.3 MB" in en; in ru, a decimal comma and Russian units; under 0.1 MB — in kilobytes. */
export function fmtSize(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  if (mb >= 100) return t("size.mb", { n: fmtNumber(Math.round(mb)) });
  if (mb >= 0.1) {
    const n = fmtNumber(mb, { minimumFractionDigits: 1, maximumFractionDigits: 1 });
    return t("size.mb", { n });
  }
  return t("size.kb", { n: fmtNumber(Math.ceil(bytes / 1024)) });
}

/** A list: "a, b, and c" in en, "a, b" plus the Russian "and" before the last item in ru. */
export function fmtList(items: readonly string[]): string {
  return new Intl.ListFormat(locale(), { style: "long", type: "conjunction" }).format(items);
}

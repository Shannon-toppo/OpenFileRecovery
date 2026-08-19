// 表示文字列の切り替え (PLAN.md 7章)。
//
// 文言は locales/*.json に分けてある。コアから来る値は `deleted` のような
// コードなので、ここで言語ごとの文言に直す。

import { derived, writable } from "svelte/store";

import en from "./locales/en.json";
import ja from "./locales/ja.json";

const dictionaries = { ja, en } as const;

/** 対応している言語。 */
export type Locale = keyof typeof dictionaries;

/** 言語の一覧(切り替え UI 用)。 */
export const locales: { id: Locale; label: string }[] = [
  { id: "ja", label: "日本語" },
  { id: "en", label: "English" },
];

const STORAGE_KEY = "ofr.locale";

function detect(): Locale {
  const saved = localStorage.getItem(STORAGE_KEY);
  if (saved === "ja" || saved === "en") return saved;
  return navigator.language.toLowerCase().startsWith("ja") ? "ja" : "en";
}

/** いまの言語。 */
export const locale = writable<Locale>(detect());

locale.subscribe((value) => localStorage.setItem(STORAGE_KEY, value));

function lookup(dict: unknown, key: string): string | undefined {
  const value = key
    .split(".")
    .reduce<unknown>((acc, part) => (acc && typeof acc === "object" ? (acc as Record<string, unknown>)[part] : undefined), dict);
  return typeof value === "string" ? value : undefined;
}

function translate(l: Locale, key: string, vars?: Record<string, unknown>): string {
  // 訳が抜けていても画面が壊れないように、日本語 → キー名の順に落とす。
  const text = lookup(dictionaries[l], key) ?? lookup(dictionaries.ja, key) ?? key;
  if (!vars) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name) =>
    name in vars ? String(vars[name]) : whole,
  );
}

/** 翻訳関数。`$t("results.title")` のように使う。 */
export const t = derived(
  locale,
  (l) =>
    (key: string, vars?: Record<string, unknown>): string =>
      translate(l, key, vars),
);

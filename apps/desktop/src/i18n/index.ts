import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import en from "./en";
import vi from "./vi";

/** UI languages. Add "zh" here + a zh.ts resource file later. */
export const LANGUAGES = [
  { code: "vi", label: "Tiếng Việt" },
  { code: "en", label: "English" },
] as const;
export type UiLang = (typeof LANGUAGES)[number]["code"];

const STORAGE_KEY = "almond.lang";

function safeStorage(): Storage | null {
  try {
    return typeof localStorage !== "undefined" ? localStorage : null;
  } catch {
    return null;
  }
}

/** Saved choice, else OS/browser locale (vi if it starts with "vi"), else English. */
export function detectLanguage(navLangs: readonly string[] = typeof navigator !== "undefined" ? navigator.languages ?? [navigator.language] : []): UiLang {
  const saved = safeStorage()?.getItem(STORAGE_KEY);
  if (saved && LANGUAGES.some((l) => l.code === saved)) return saved as UiLang;
  for (const l of navLangs) {
    const base = l.toLowerCase().split("-")[0];
    if (LANGUAGES.some((x) => x.code === base)) return base as UiLang;
  }
  return "en";
}

export function setLanguage(lang: UiLang) {
  try {
    safeStorage()?.setItem(STORAGE_KEY, lang);
  } catch {
    /* ignore */
  }
  if (typeof document !== "undefined") document.documentElement.lang = lang;
  return i18n.changeLanguage(lang);
}

export function initI18n(lang: UiLang = detectLanguage()) {
  if (!i18n.isInitialized) {
    void i18n.use(initReactI18next).init({
      resources: { en: { translation: en }, vi: { translation: vi } },
      lng: lang,
      fallbackLng: "en",
      interpolation: { escapeValue: false },
      returnNull: false,
    });
  } else {
    void i18n.changeLanguage(lang);
  }
  if (typeof document !== "undefined") document.documentElement.lang = lang;
  return i18n;
}

export default i18n;

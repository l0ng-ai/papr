// The target languages offered for article translation, shown in both the
// Settings default selector and the reader's inline (Chrome-style) switcher.
//
// Codes are the app's canonical form (BCP-47-ish); the Rust backend's
// `translate_api::provider_lang_code` maps each one to the code the chosen
// provider (DeepL / Google / a compatible endpoint) actually expects. Labels are
// in each language's own script, the way a browser's "Translate to…" menu shows
// them. Not every provider supports every language (DeepL's set is the
// narrowest); an unsupported pick simply surfaces the provider's error.

export interface TranslateLanguage {
  code: string;
  label: string;
}

export const TRANSLATE_LANGUAGES: TranslateLanguage[] = [
  { code: "ar", label: "العربية" },
  { code: "bg", label: "Български" },
  { code: "cs", label: "Čeština" },
  { code: "da", label: "Dansk" },
  { code: "de", label: "Deutsch" },
  { code: "el", label: "Ελληνικά" },
  { code: "en", label: "English" },
  { code: "es", label: "Español" },
  { code: "et", label: "Eesti" },
  { code: "fi", label: "Suomi" },
  { code: "fr", label: "Français" },
  { code: "he", label: "עברית" },
  { code: "hi", label: "हिन्दी" },
  { code: "hu", label: "Magyar" },
  { code: "id", label: "Indonesia" },
  { code: "it", label: "Italiano" },
  { code: "ja", label: "日本語" },
  { code: "ko", label: "한국어" },
  { code: "lt", label: "Lietuvių" },
  { code: "lv", label: "Latviešu" },
  { code: "nl", label: "Nederlands" },
  { code: "no", label: "Norsk" },
  { code: "pl", label: "Polski" },
  { code: "pt", label: "Português" },
  { code: "pt-BR", label: "Português (Brasil)" },
  { code: "ro", label: "Română" },
  { code: "ru", label: "Русский" },
  { code: "sk", label: "Slovenčina" },
  { code: "sl", label: "Slovenščina" },
  { code: "sv", label: "Svenska" },
  { code: "th", label: "ไทย" },
  { code: "tr", label: "Türkçe" },
  { code: "uk", label: "Українська" },
  { code: "vi", label: "Tiếng Việt" },
  { code: "zh-Hans", label: "简体中文" },
  { code: "zh-Hant", label: "繁體中文" },
];

/** The label for a language code, falling back to the code itself. */
export function translateLanguageLabel(code: string): string {
  return TRANSLATE_LANGUAGES.find((l) => l.code === code)?.label ?? code;
}

/** Map a legacy/UI language code to the closest translation target code, so a
 *  value inherited from the interface language (or an older setting) resolves to
 *  an entry in the list above. */
export function normalizeTargetLang(code: string): string {
  if (code === "zh") return "zh-Hans";
  if (TRANSLATE_LANGUAGES.some((l) => l.code === code)) return code;
  // Fall back to the base subtag when an unknown region tag is given.
  const base = code.split("-")[0];
  return TRANSLATE_LANGUAGES.some((l) => l.code === base) ? base : "en";
}

// teamx i18n: locale detection + translation function.
//
// Detects system locale via LANG/LC_MESSAGES env var.
// Falls back to 'en' if not Chinese.

import zh from "./zh.json"
import en from "./en.json"

export type Locale = "zh" | "en"

const messages: Record<Locale, Record<string, string>> = { zh, en }

/** Detect the user's system locale. Returns 'zh' for Chinese, 'en' for everything else. */
export function detectLocale(): Locale {
  const lang = process.env.LANG || process.env.LC_MESSAGES || "en"
  return lang.startsWith("zh") ? "zh" : "en"
}

/** Current locale (detected once at import time). */
export const locale: Locale = detectLocale()

/**
 * Look up a translation key. Supports simple `{var}` interpolation.
 * Falls back to English, then to the key itself.
 *
 * @example
 *   t("toast.team_created", { name: "My Team" })
 *   // zh: '#${seq} 团队「My Team」已创建'
 *   // en: '#${seq} Team "My Team" created'
 */
export function t(key: string, vars?: Record<string, string>): string {
  let template = messages[locale]?.[key] ?? messages.en[key] ?? key
  if (vars) {
    for (const [k, v] of Object.entries(vars)) {
      template = template.replaceAll(`{${k}}`, v)
    }
  }
  return template
}

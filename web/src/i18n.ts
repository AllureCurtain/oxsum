import i18n from "i18next";
import LanguageDetector from "i18next-browser-languagedetector";
import { initReactI18next } from "react-i18next";

import en from "./locales/en.json";
import zhCN from "./locales/zh-CN.json";

// Console i18n (issue #507): English is the source of truth and the fallback;
// zh-CN is the shipped second language. The detector prefers the viewer's
// stored choice (`localStorage["i18nextLng"]`, written by the header switcher)
// over the browser's `navigator.language`, so a picked language survives
// reloads while a first visit follows the browser.
void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: {
      en: { translation: en },
      "zh-CN": { translation: zhCN },
    },
    fallbackLng: "en",
    supportedLngs: ["en", "zh-CN"],
    nonExplicitSupportedLngs: true, // "zh"/"zh-Hans" resolve to zh-CN
    returnEmptyString: false,
    interpolation: { escapeValue: false }, // React already escapes
    detection: {
      order: ["localStorage", "navigator"],
      lookupLocalStorage: "i18nextLng",
      caches: ["localStorage"],
    },
  });

export default i18n;

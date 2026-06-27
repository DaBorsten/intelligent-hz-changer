import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import LanguageDetector from "i18next-browser-languagedetector";

import de from "./locales/de/translation.json";
import en from "./locales/en/translation.json";

void i18n
  .use(LanguageDetector)
  .use(initReactI18next)
  .init({
    resources: { de: { translation: de }, en: { translation: en } },
    fallbackLng: "de",
    supportedLngs: ["de", "en"],
    detection: {
      order: ["localStorage"],
      lookupLocalStorage: "hz-language",
      caches: ["localStorage"],
    },
    interpolation: { escapeValue: false },
  });

export default i18n;

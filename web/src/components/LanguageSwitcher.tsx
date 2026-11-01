import { useTranslation } from "react-i18next";

// The console language switch (issue #507): a quiet EN/中文 toggle that rides
// the header's other controls. The choice persists through the detector's
// `localStorage["i18nextLng"]` cache — a first visit defaults to the browser
// language; a picked one survives reloads.
export function LanguageSwitcher() {
  const { i18n } = useTranslation();
  const current = i18n.language?.startsWith("zh") ? "zh-CN" : "en";
  return (
    <div
      role="group"
      aria-label="Language / 语言"
      className="flex items-center rounded-lg border border-line bg-surface text-xs"
    >
      {(
        [
          ["en", "EN"],
          ["zh-CN", "中文"],
        ] as const
      ).map(([lng, label]) => (
        <button
          key={lng}
          type="button"
          aria-pressed={current === lng}
          onClick={() => void i18n.changeLanguage(lng)}
          className={`min-h-11 rounded-md px-2.5 py-1.5 transition-colors sm:min-h-0 ${
            current === lng
              ? "font-semibold text-primary"
              : "text-muted hover:text-ink"
          }`}
        >
          {label}
        </button>
      ))}
    </div>
  );
}

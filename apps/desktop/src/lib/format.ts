import { useTranslation } from "react-i18next";

const locale = (lang: string) => (lang === "vi" ? "vi-VN" : lang === "zh" ? "zh-CN" : "en-GB");

export function useFormat() {
  const { i18n, t } = useTranslation();
  const loc = locale(i18n.language);
  return {
    time: (ms: number) => new Date(ms).toLocaleTimeString(loc, { hour: "2-digit", minute: "2-digit" }),
    date: (ms: number) => new Date(ms).toLocaleDateString(loc, { weekday: "short", day: "numeric", month: "short" }),
    dateTime: (ms: number) =>
      new Date(ms).toLocaleString(loc, { weekday: "short", day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" }),
    day: (ms: number) => {
      const d = new Date(ms);
      const today = new Date();
      today.setHours(0, 0, 0, 0);
      const start = new Date(d);
      start.setHours(0, 0, 0, 0);
      const diff = Math.round((today.getTime() - start.getTime()) / 86_400_000);
      if (diff === 0) return t("home.today");
      if (diff === 1) return t("home.yesterday");
      return d.toLocaleDateString(loc, { weekday: "long", day: "numeric", month: "long" });
    },
    relative: (ms: number, now = Date.now()) => {
      const rtf = new Intl.RelativeTimeFormat(loc, { numeric: "auto" });
      const s = Math.round((ms - now) / 1000);
      const abs = Math.abs(s);
      if (abs < 60) return rtf.format(s, "second");
      if (abs < 3600) return rtf.format(Math.round(s / 60), "minute");
      if (abs < 86400) return rtf.format(Math.round(s / 3600), "hour");
      return rtf.format(Math.round(s / 86400), "day");
    },
  };
}

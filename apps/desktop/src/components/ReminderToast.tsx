import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "react-router";
import type { MeetingReminder } from "../gen/MeetingReminder";
import { useApi, useNotification } from "../api/context";
import { errorMessage } from "../api/errors";
import { startNotes } from "../lib/startNotes";
import { useAppState } from "../state/AppState";
import { Button, Icon } from "./ui";

/** Toast for `calendar.reminder` with a one-click "Start notes". */
export function ReminderToast() {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const { platform, recording } = useAppState();
  const [reminders, setReminders] = useState<MeetingReminder[]>([]);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useNotification("calendar.reminder", (r) =>
    setReminders((list) => (list.some((x) => x.event.event_id === r.event.event_id) ? list : [...list, r])),
  );

  const r = reminders[0];
  if (!r) return null;
  const dismiss = () => setReminders((l) => l.slice(1));

  const start = async () => {
    setBusy(true);
    setErr(null);
    try {
      const id = await startNotes(api, { event: r.event, platform });
      dismiss();
      nav(`/meeting/${id}`);
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div role="status" aria-live="polite" className="fixed inset-x-3 bottom-20 z-50 mx-auto max-w-md md:bottom-6 md:right-6 md:left-auto md:mx-0 md:w-96">
      <div className="rounded-2xl border border-line bg-surface p-4 shadow-xl shadow-black/10">
        <div className="flex items-start gap-3">
          <span className="mt-0.5 rounded-full bg-accent-soft p-2 text-accent">
            <Icon.bell />
          </span>
          <div className="min-w-0 flex-1">
            <p className="truncate font-medium">{t("reminder.title", { title: r.event.title })}</p>
            <p className="text-sm text-muted">{t("reminder.body", { count: r.minutes_before })}</p>
            {err && <p className="mt-1 text-sm text-bad">{err}</p>}
          </div>
        </div>
        <div className="mt-3 flex justify-end gap-2">
          <Button variant="ghost" size="sm" onClick={dismiss}>
            {t("reminder.dismiss")}
          </Button>
          <Button variant="primary" size="sm" disabled={busy || recording.state !== "idle"} onClick={start}>
            <Icon.mic width={16} height={16} />
            {t("home.startNotes")}
          </Button>
        </div>
      </div>
    </div>
  );
}

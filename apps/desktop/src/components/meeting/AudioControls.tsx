import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AudioState } from "../../api/types";
import { useApi } from "../../api/context";
import { errorMessage } from "../../api/errors";
import { useFormat } from "../../lib/format";
import { Button, cx, Icon } from "../ui";

/**
 * Next to the audio player: ⭐ keep the recording (automatic cleanup skips it) and "delete audio,
 * keep the text". Once deleted, a short note replaces both.
 */
export function AudioControls({
  meetingId,
  hasAudio,
  hasTranscript,
  onDeleted,
}: {
  meetingId: string;
  hasAudio: boolean;
  hasTranscript: boolean;
  onDeleted: () => void;
}) {
  const { t } = useTranslation();
  const api = useApi();
  const f = useFormat();
  const [st, setSt] = useState<AudioState | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    api
      .call("storage.audioState", { id: meetingId })
      .then((s) => alive && setSt(s))
      .catch(() => alive && setSt(null));
    return () => {
      alive = false;
    };
  }, [api, meetingId]);

  if (!st) return null;
  if (st.deleted_at != null) {
    return (
      <p className="text-xs text-muted" data-testid="audio-deleted">
        {t(st.deleted_reason === "retention" ? "storage.deletedAuto" : "storage.deletedOn", { date: f.date(st.deleted_at) })}
      </p>
    );
  }
  if (!hasAudio) return null;

  const toggleKeep = async () => {
    const keep = !st.keep;
    setSt({ ...st, keep });
    await api.call("storage.keepAudio", { meeting_id: meetingId, keep }).catch((e) => setErr(errorMessage(e)));
  };
  const del = async () => {
    if (!window.confirm(t("storage.deleteConfirm"))) return;
    setBusy(true);
    setErr(null);
    try {
      const r = await api.call("storage.deleteAudio", { meeting_ids: [meetingId] });
      if (r.deleted === 0 && r.skipped.length) setErr(r.skipped[0][1]);
      else {
        setSt({ ...st, deleted_at: Date.now(), deleted_reason: "manual" });
        onDeleted();
      }
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      <Button
        size="sm"
        variant="ghost"
        onClick={() => void toggleKeep()}
        aria-pressed={st.keep}
        title={t("storage.keepHint")}
        className={cx(st.keep && "text-accent")}
      >
        <span aria-hidden>{st.keep ? "★" : "☆"}</span>
        {t(st.keep ? "storage.kept" : "storage.keep")}
      </Button>
      <Button
        size="sm"
        variant="ghost"
        onClick={() => void del()}
        disabled={busy || !hasTranscript || st.keep}
        title={!hasTranscript ? t("storage.needsTranscript") : st.keep ? t("storage.unkeepFirst") : t("storage.deleteHint")}
      >
        <Icon.trash width={14} height={14} />
        {t("storage.deleteAudio")}
      </Button>
      {err && <span className="text-bad">{err}</span>}
    </div>
  );
}

// The cookbook: every recipe as a card (built-ins and the user's own), create / edit (editing a
// built-in's text saves a copy), hide built-ins, delete own recipes, auto-run, "Run on…" a meeting.
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import type { Meeting } from "../gen/Meeting";
import { useApi, useLoad } from "../api/context";
import { errorMessage } from "../api/errors";
import type { RecipeInfo, RecipeSaveParams } from "../api/types";
import { useFormat } from "../lib/format";
import { useRecipeText } from "../components/recipes/recipeText";
import { Button, ErrorNote, Icon, Loading, Pill, Toggle } from "../components/ui";

type Draft = { id: string | null; builtin: boolean; name: string; icon: string; description: string; prompt: string; auto_run: boolean };

function RecipeEditor({ draft, onCancel, onSaved }: { draft: Draft; onCancel: () => void; onSaved: (r: RecipeInfo) => void }) {
  const { t } = useTranslation();
  const api = useApi();
  const [d, setD] = useState(draft);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const set = <K extends keyof Draft>(k: K, v: Draft[K]) => setD((x) => ({ ...x, [k]: v }));
  const save = async () => {
    setErr(null);
    setBusy(true);
    try {
      // Send only what changed: a built-in shows its name in the UI language, and saving it
      // unchanged must not turn into a copy.
      const p: RecipeSaveParams = { id: d.id };
      const keys = ["name", "icon", "description", "prompt", "auto_run"] as const;
      // An edited built-in becomes a copy with everything as shown in the editor.
      const textChanged = keys.some((k) => k !== "auto_run" && d[k] !== draft[k]);
      for (const k of keys) if (!d.id || d[k] !== draft[k] || (textChanged && k !== "auto_run")) Object.assign(p, { [k]: d[k] });
      onSaved(await api.call("recipes.save", p));
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const field = "w-full rounded-xl border border-line bg-bg px-3 py-2 text-sm";
  return (
    <form
      className="flex flex-col gap-3 rounded-2xl border border-accent/40 bg-surface p-4 sm:p-5"
      aria-label={d.id ? t("recipes.editorEdit") : t("recipes.editorNew")}
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <p className="text-sm font-semibold">{d.id ? t("recipes.editorEdit") : t("recipes.editorNew")}</p>
      <div className="flex gap-3">
        <label className="flex w-24 flex-col gap-1 text-xs text-muted">
          {t("recipes.icon")}
          <input value={d.icon} onChange={(e) => set("icon", e.target.value)} maxLength={8} className={field} />
        </label>
        <label className="flex min-w-0 flex-1 flex-col gap-1 text-xs text-muted">
          {t("recipes.name")}
          <input value={d.name} onChange={(e) => set("name", e.target.value)} required className={field} />
        </label>
      </div>
      <label className="flex flex-col gap-1 text-xs text-muted">
        {t("recipes.description")}
        <input value={d.description} onChange={(e) => set("description", e.target.value)} className={field} />
      </label>
      <div className="flex flex-col gap-1 text-xs text-muted">
        <label className="flex flex-col gap-1">
          {t("recipes.prompt")}
          <textarea value={d.prompt} onChange={(e) => set("prompt", e.target.value)} required rows={8} className={`${field} font-mono text-[13px] leading-relaxed`} />
        </label>
        <span className="text-faint">{t("recipes.promptHint")}</span>
      </div>
      <label className="flex items-center justify-between gap-3 text-sm">
        {t("recipes.autoRun")}
        <Toggle checked={d.auto_run} onChange={(v) => set("auto_run", v)} label={t("recipes.autoRun")} />
      </label>
      {d.builtin && <p className="rounded-xl bg-sunken p-2 text-xs text-muted">{t("recipes.builtinCopyNote")}</p>}
      {err && <p className="text-sm text-bad">{err}</p>}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onCancel}>
          {t("recipes.cancel")}
        </Button>
        <Button variant="primary" type="submit" disabled={busy || !d.name.trim() || !d.prompt.trim()}>
          {t("recipes.save")}
        </Button>
      </div>
    </form>
  );
}

function RunOn({ recipe, meetings, onClose }: { recipe: RecipeInfo; meetings: Meeting[]; onClose: () => void }) {
  const { t } = useTranslation();
  const api = useApi();
  const f = useFormat();
  const text = useRecipeText();
  const [meetingId, setMeetingId] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string; meeting?: string } | null>(null);
  const name = text(recipe).name;
  const run = async () => {
    const m = meetings.find((x) => x.id === meetingId);
    if (!m) return;
    try {
      await api.call("recipes.run", { recipe_id: recipe.id, meeting_id: m.id });
      setMsg({ ok: true, text: t("recipes.queued", { name, meeting: m.title || t("recipes.untitled") }), meeting: m.id });
    } catch (e) {
      setMsg({ ok: false, text: errorMessage(e) });
    }
  };
  return (
    <div className="mt-3 flex flex-col gap-2 border-t border-line pt-3">
      {meetings.length === 0 ? (
        <p className="text-sm text-muted">{t("recipes.noMeetings")}</p>
      ) : (
        <div className="flex flex-wrap items-center gap-2">
          <select
            aria-label={t("recipes.runOnMeeting", { name })}
            value={meetingId}
            onChange={(e) => setMeetingId(e.target.value)}
            className="min-w-0 flex-1 rounded-full border border-line bg-surface px-3 py-1.5 text-sm"
          >
            <option value="">{t("recipes.chooseMeeting")}</option>
            {meetings.map((m) => (
              <option key={m.id} value={m.id}>
                {(m.title || t("recipes.untitled")) + " · " + f.date(m.started_at)}
              </option>
            ))}
          </select>
          <Button size="sm" variant="primary" disabled={!meetingId} onClick={() => void run()}>
            {t("recipes.run")}
          </Button>
          <Button size="sm" variant="ghost" onClick={onClose}>
            {t("recipes.cancel")}
          </Button>
        </div>
      )}
      {msg && (
        <p className={msg.ok ? "text-sm text-muted" : "text-sm text-bad"} role="status">
          {msg.text}{" "}
          {msg.meeting && (
            <Link to={`/meeting/${msg.meeting}`} className="text-accent hover:underline">
              {t("recipes.openMeeting")} →
            </Link>
          )}
        </p>
      )}
    </div>
  );
}

function RecipeCard({ r, meetings, onEdit, onChanged }: { r: RecipeInfo; meetings: Meeting[]; onEdit: () => void; onChanged: () => void }) {
  const { t } = useTranslation();
  const api = useApi();
  const text = useRecipeText();
  const [runOpen, setRunOpen] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const { name, description } = text(r);
  const act = (fn: () => Promise<unknown>) => () =>
    void fn()
      .then(onChanged)
      .catch((e) => setErr(errorMessage(e)));
  return (
    <li className={`flex flex-col rounded-2xl border border-line bg-surface p-4 ${r.hidden ? "opacity-60" : ""}`} aria-label={name} data-recipe={r.id}>
      <div className="flex items-start gap-3">
        <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-xl bg-sunken text-xl" aria-hidden>
          {r.icon || "🍳"}
        </span>
        <div className="min-w-0 flex-1">
          <p className="font-semibold">{name}</p>
          {description && <p className="mt-0.5 text-sm text-muted">{description}</p>}
          <div className="mt-2 flex flex-wrap gap-1.5">
            {r.builtin && <Pill>{t("recipes.builtinBadge")}</Pill>}
            {r.auto_run && <Pill className="bg-accent-soft text-accent">{t("recipes.autoRunBadge")}</Pill>}
            {r.hidden && <Pill>{t("recipes.hiddenBadge")}</Pill>}
          </div>
        </div>
      </div>
      <div className="mt-3 flex flex-wrap gap-1.5">
        <Button size="sm" variant="primary" onClick={() => setRunOpen((o) => !o)} aria-expanded={runOpen}>
          {t("recipes.runOn")}
        </Button>
        <Button size="sm" variant="ghost" onClick={onEdit}>
          {t("recipes.edit")}
        </Button>
        <Button size="sm" variant="ghost" onClick={act(() => api.call("recipes.save", { id: r.id, auto_run: !r.auto_run }))} aria-pressed={r.auto_run}>
          {t("recipes.autoRunBadge")}
        </Button>
        {r.builtin ? (
          <Button size="sm" variant="ghost" onClick={act(() => api.call("recipes.save", { id: r.id, hidden: !r.hidden }))}>
            {r.hidden ? t("recipes.unhide") : t("recipes.hide")}
          </Button>
        ) : (
          <Button
            size="sm"
            variant="ghost"
            className="text-bad"
            onClick={() => {
              if (window.confirm(t("recipes.deleteConfirm", { name }))) act(() => api.call("recipes.delete", { id: r.id }))();
            }}
          >
            {t("recipes.delete")}
          </Button>
        )}
      </div>
      {err && <p className="mt-2 text-sm text-bad">{err}</p>}
      {runOpen && <RunOn recipe={r} meetings={meetings} onClose={() => setRunOpen(false)} />}
    </li>
  );
}

export function Cookbook() {
  const { t } = useTranslation();
  const api = useApi();
  const text = useRecipeText();
  const recipes = useLoad(() => api.call("recipes.list", {}), [api]);
  const meetings = useLoad(() => api.call("meetings.list", { text: null, from: null, to: null, tag: null, limit: 30, offset: null }), [api]);
  const [editing, setEditing] = useState<Draft | null>(null);
  const [showHidden, setShowHidden] = useState(false);

  if (recipes.error)
    return (
      <div className="p-6">
        <ErrorNote error={recipes.error} onRetry={recipes.reload} />
      </div>
    );
  if (!recipes.data) return <Loading />;
  const all = recipes.data;
  const hiddenCount = all.filter((r) => r.hidden).length;
  const shown = all.filter((r) => showHidden || !r.hidden);
  const done = (m: Meeting) => m.ended_at != null;
  const edit = (r: RecipeInfo) => {
    const { name, description } = text(r);
    setEditing({ id: r.id, builtin: r.builtin, name, icon: r.icon ?? "", description, prompt: r.prompt, auto_run: r.auto_run });
  };

  return (
    <div className="mx-auto flex max-w-4xl flex-col gap-6 px-4 py-6 sm:px-8">
      <header className="flex flex-wrap items-end gap-3">
        <div className="min-w-0 flex-1">
          <h1 className="text-2xl font-semibold tracking-tight">{t("recipes.title")}</h1>
          <p className="mt-1 text-sm text-muted">{t("recipes.subtitle")}</p>
        </div>
        <Button variant="primary" onClick={() => setEditing({ id: null, builtin: false, name: "", icon: "", description: "", prompt: "", auto_run: false })}>
          <Icon.plus width={16} height={16} />
          {t("recipes.newRecipe")}
        </Button>
      </header>

      {editing && (
        <RecipeEditor
          key={editing.id ?? "new"}
          draft={editing}
          onCancel={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            void recipes.reload();
          }}
        />
      )}

      <ul className="grid gap-3 sm:grid-cols-2" aria-label={t("recipes.title")}>
        {shown.map((r) => (
          <RecipeCard key={r.id} r={r} meetings={(meetings.data ?? []).filter(done)} onEdit={() => edit(r)} onChanged={() => void recipes.reload()} />
        ))}
      </ul>

      {hiddenCount > 0 && (
        <label className="flex w-fit items-center gap-2 text-sm text-muted">
          <input type="checkbox" checked={showHidden} onChange={(e) => setShowHidden(e.target.checked)} />
          {t("recipes.showHidden")} ({hiddenCount})
        </label>
      )}
    </div>
  );
}

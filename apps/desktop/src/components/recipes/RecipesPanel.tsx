// The meeting view's "Recipes" tab: run a recipe on this meeting, and read the results
// (Markdown with citation chips that jump to the transcript), with "Run again".
import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import type { Artifact } from "../../gen/Artifact";
import type { JobProgress } from "../../gen/JobProgress";
import { useApi } from "../../api/context";
import { errorMessage } from "../../api/errors";
import type { RecipeInfo } from "../../api/types";
import { shortModel } from "../../lib/artifacts";
import { useFormat } from "../../lib/format";
import { Button, Spinner } from "../ui";
import { Markdown } from "./Markdown";
import { liveOutputs, outputBody, useRecipeText } from "./recipeText";

const active = (j: JobProgress) => j.job.processor === "recipe.llm" && (j.status === "pending" || j.status === "claimed");
const jobRecipe = (j: JobProgress) => (j.job.params as { recipe_id?: string } | null)?.recipe_id ?? "";

export function RecipesPanel({ meetingId, artifacts, jobs, hasTranscript }: { meetingId: string; artifacts: Artifact[]; jobs: JobProgress[]; hasTranscript: boolean }) {
  const { t } = useTranslation();
  const api = useApi();
  const f = useFormat();
  const text = useRecipeText();
  const [recipes, setRecipes] = useState<RecipeInfo[]>([]);
  const [choice, setChoice] = useState("");
  const [extra, setExtra] = useState("");
  const [err, setErr] = useState<string | null>(null);
  // Runs requested here: recipe id while the call is in flight, then job id -> recipe id.
  const [starting, setStarting] = useState<Set<string>>(new Set());
  const [mine, setMine] = useState<Map<string, string>>(new Map());

  useEffect(() => {
    let alive = true;
    api
      .call("recipes.list", {})
      .then((l) => alive && setRecipes(l))
      .catch((e) => alive && setErr(errorMessage(e)));
    return () => {
      alive = false;
    };
  }, [api]);

  const outputs = useMemo(() => liveOutputs(artifacts), [artifacts]);
  const running = useMemo(() => new Set(jobs.filter(active).map(jobRecipe)), [jobs]);
  const finished = (jid: string) => jobs.some((j) => j.job.id === jid && !active(j));
  // A run requested here is "running" until its job is reported finished.
  const busy = (id: string) => running.has(id) || starting.has(id) || [...mine].some(([jid, rid]) => rid === id && !finished(jid));

  const visible = recipes.filter((r) => !r.hidden);
  const byId = (id: string) => recipes.find((r) => r.id === id);

  const run = async (recipe_id: string, withExtra?: string) => {
    setErr(null);
    setStarting((s) => new Set(s).add(recipe_id));
    try {
      const job = await api.call("recipes.run", { recipe_id, meeting_id: meetingId, extra: withExtra?.trim() || null });
      setMine((m) => new Map(m).set(job.id, recipe_id));
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setStarting((s) => {
        const n = new Set(s);
        n.delete(recipe_id);
        return n;
      });
    }
  };

  return (
    <div className="flex flex-col gap-5" data-testid="recipes-panel">
      <div className="flex flex-col gap-2 rounded-2xl border border-line bg-surface p-3">
        <div className="flex flex-wrap items-center gap-2">
          <select
            aria-label={t("recipes.pickRecipe")}
            value={choice}
            onChange={(e) => setChoice(e.target.value)}
            disabled={!hasTranscript}
            className="min-w-0 flex-1 rounded-full border border-line bg-surface px-3 py-1.5 text-sm"
          >
            <option value="">{t("recipes.pickRecipe")}</option>
            {visible.map((r) => (
              <option key={r.id} value={r.id}>
                {r.icon ? `${r.icon} ` : ""}
                {text(r).name}
              </option>
            ))}
          </select>
          <Button size="sm" variant="primary" disabled={!hasTranscript || !choice || busy(choice)} onClick={() => void run(choice, extra).then(() => setExtra(""))}>
            {choice && busy(choice) ? <Spinner /> : null}
            {t("recipes.runRecipe")}
          </Button>
        </div>
        {choice && (
          <input
            value={extra}
            onChange={(e) => setExtra(e.target.value)}
            aria-label={t("recipes.extra")}
            placeholder={`${t("recipes.extra")} — ${t("recipes.extraPlaceholder")}`}
            className="rounded-full border border-line bg-bg px-3 py-1.5 text-sm"
          />
        )}
        {!hasTranscript && <p className="text-sm text-muted">{t("recipes.needsTranscript")}</p>}
        {err && <p className="text-sm text-bad">{err}</p>}
        <Link to="/cookbook" className="w-fit text-xs text-accent hover:underline">
          {t("recipes.manage")} →
        </Link>
      </div>

      {outputs.length === 0 ? (
        <p className="text-sm text-muted">{t("recipes.noOutputs")}</p>
      ) : (
        outputs.map((a) => {
          const o = outputBody(a);
          const r = byId(o.recipe_id);
          const name = r ? text(r).name : o.recipe_name;
          return (
            <section key={a.id} className="rounded-2xl border border-line bg-surface p-4 sm:p-5" aria-label={name} data-recipe={o.recipe_id}>
              <header className="mb-3 flex flex-wrap items-center gap-2">
                {r?.icon && <span aria-hidden>{r.icon}</span>}
                <h3 className="text-base font-semibold">{name}</h3>
                <span className="text-xs text-muted">{t("recipes.ranAt", { time: f.dateTime(a.created_at.wall_ms), model: shortModel(a.provenance.model) })}</span>
                <Button size="sm" variant="ghost" className="ml-auto" disabled={busy(o.recipe_id) || !hasTranscript || !r} onClick={() => void run(o.recipe_id)}>
                  {busy(o.recipe_id) ? <Spinner /> : null}
                  {busy(o.recipe_id) ? t("recipes.running") : t("recipes.rerun")}
                </Button>
              </header>
              <Markdown text={o.markdown} citations={o.citations} />
            </section>
          );
        })
      )}
    </div>
  );
}

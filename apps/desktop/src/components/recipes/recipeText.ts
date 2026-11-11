import { useCallback } from "react";
import { useTranslation } from "react-i18next";
import type { Artifact } from "../../gen/Artifact";
import type { RecipeOutput } from "../../gen/RecipeOutput";
import type { Recipe } from "../../gen/Recipe";
import { Kinds } from "../../api/types";

/** Built-in recipes get their name and description in the UI language; user recipes as written. */
export function useRecipeText() {
  const { t } = useTranslation();
  return useCallback(
    (r: Pick<Recipe, "id" | "name" | "description" | "builtin">) => {
      if (!r.builtin || !r.id.startsWith("builtin:")) return { name: r.name, description: r.description };
      const key = r.id.slice("builtin:".length);
      return {
        name: t(`recipes.builtin.${key}.name`, { defaultValue: r.name }),
        description: t(`recipes.builtin.${key}.description`, { defaultValue: r.description }),
      };
    },
    [t],
  );
}

export const outputBody = (a: Artifact) => a.body as unknown as RecipeOutput;

const newerFirst = (a: Artifact, b: Artifact) => b.created_at.wall_ms - a.created_at.wall_ms || b.created_at.logical - a.created_at.logical;

/** Live recipe outputs (not superseded), newest first, one per recipe. */
export function liveOutputs(artifacts: Artifact[]): Artifact[] {
  const all = artifacts.filter((a) => a.kind === Kinds.recipeOutput);
  const superseded = new Set(all.map((a) => a.supersedes).filter(Boolean));
  const seen = new Set<string>();
  return all
    .filter((a) => !superseded.has(a.id))
    .sort(newerFirst)
    .filter((a) => {
      const id = outputBody(a).recipe_id;
      if (seen.has(id)) return false;
      seen.add(id);
      return true;
    });
}

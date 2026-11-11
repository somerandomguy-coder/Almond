// Mock data for the cookbook (recipes.*): the built-ins as the core ships them and canned
// answers with citations into a transcript.
import type { Recipe } from "../../gen/Recipe";
import type { RecipeOutput } from "../../gen/RecipeOutput";
import type { Segment } from "../../gen/Segment";
import type { RecipeInfo } from "../types";

const B = (id: string, name: string, description: string, icon: string, prompt: string): RecipeInfo => ({
  id: `builtin:${id}`,
  name,
  description,
  icon,
  prompt,
  auto_run: false,
  builtin: true,
  updated_at: 0,
  hidden: false,
});

/** Same ids, names and descriptions as `almond_core::recipes::builtins` (prompts shortened). */
export function builtinRecipes(): RecipeInfo[] {
  return [
    B("teach_back", "Teach it back to me", "Explains the meeting as if teaching you, with key concepts and a short quiz.", "🎓", "Teach me what happened in this meeting as if I were a smart newcomer who missed it…"),
    B("forgot_to_ask", "Questions I forgot to ask", "Gaps, unclear decisions and risks, plus follow-up questions for each person.", "❓", "Find what was left unclear or unasked in this meeting…"),
    B("communication", "How did I communicate?", "Talk time, interruptions, filler words and clarity, with one thing to try next time.", "🗣️", "Give me (the speaker \"me\") honest, specific feedback on how I communicated…"),
    B("decisions", "Decisions & owners", "A table of every decision with its owner and due date.", "✅", "List every decision made in this meeting as a Markdown table…"),
    B("followup_email", "Follow-up email draft", "A ready-to-edit follow-up email to the participants.", "✉️", "Draft a follow-up email from me to the other participants…"),
  ];
}

/** A canned answer for `recipe`, citing transcript segments as `[n]` → `citations[n-1]`. */
export function mockRecipeOutput(recipe: Recipe, transcriptId: string, segs: Segment[], extra?: string | null): RecipeOutput {
  const pick = [1, 2, 3, 8].filter((i) => i < segs.length);
  const citations = pick.map((i) => ({ artifact: transcriptId, segment: segs[i].idx, chars: null, time_ms: [segs[i].t0_ms, segs[i].t1_ms] as [number, number] }));
  const c = (n: number) => (n <= citations.length ? `[${n}]` : "");
  const me = segs.filter((s) => s.speaker === "me");
  const talk = (f: (s: Segment) => boolean) => segs.filter(f).reduce((a, s) => a + (s.t1_ms - s.t0_ms), 0);
  const total = talk(() => true) || 1;
  const share = Math.round((talk((s) => s.speaker === "me") / total) * 100);
  let markdown: string;
  switch (recipe.id) {
    case "builtin:communication":
      markdown = `## By the numbers\n\n| Speaker | Talk time | Share | Turns |\n|---|---|---|---|\n| me | ${me.length} lines | ${share}% | ${me.length} |\n| others | ${segs.length - me.length} lines | ${100 - share}% | ${segs.length - me.length} |\n\nYou spoke for about ${share}% of the meeting.\n\n## What went well\n- You opened with a clear agenda ${c(1)}.\n- You delegated the job description explicitly ${c(3)}.\n\n## One thing to try next time\nSummarize the decisions out loud before closing.`;
      break;
    case "builtin:decisions":
      markdown = `| Decision | Owner | Due date | Source |\n|---|---|---|---|\n| Marketing budget is 200 million VND | — | — | ${c(1)} |\n| Hire two backend engineers | David | December | ${c(2)}${c(3)} |\n| Send the detailed plan | Lan | Friday | ${c(4)} |\n\n**Open questions**\n- QA is short of a tester; nobody owns hiring one yet.`;
      break;
    default:
      markdown = `## ${recipe.name}\n\n- The marketing budget is **200 million VND**, up 15% from Q3 ${c(1)}.\n- The team wants two backend engineers by December ${c(2)}.\n- David drafts the job description this week ${c(3)}.\n- Lan sends the detailed plan before Friday ${c(4)}.`;
  }
  if (extra?.trim()) markdown += `\n\n_Focus: ${extra.trim()}_`;
  return { recipe_id: recipe.id, recipe_name: recipe.name, markdown, citations };
}

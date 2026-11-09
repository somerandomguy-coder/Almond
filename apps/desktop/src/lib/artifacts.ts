import type { ActionItems } from "../gen/ActionItems";
import type { ActionProposals } from "../gen/ActionProposals";
import type { Artifact } from "../gen/Artifact";
import type { Claims } from "../gen/Claims";
import type { EnhancedNotes } from "../gen/EnhancedNotes";
import type { Summary } from "../gen/Summary";
import type { Transcript } from "../gen/Transcript";
import type { Verdict } from "../gen/Verdict";
import type { VerdictLabel } from "../gen/VerdictLabel";
import type { Verdicts } from "../gen/Verdicts";
import { Kinds } from "../api/types";

/** Typed views of well-known artifact bodies (schema 1). Unknown kinds stay opaque JSON. */
export interface BodyByKind {
  transcript: Transcript;
  summary: Summary;
  enhanced_notes: EnhancedNotes;
  action_items: ActionItems;
  claims: Claims;
  verdicts: Verdicts;
  action_proposals: ActionProposals;
}

export function body<K extends keyof BodyByKind>(a: Artifact, _kind: K): BodyByKind[K] {
  return a.body as unknown as BodyByKind[K];
}

const newerFirst = (a: Artifact, b: Artifact) =>
  b.created_at.wall_ms - a.created_at.wall_ms || b.created_at.logical - a.created_at.logical || (b.id > a.id ? 1 : -1);

/** Latest artifact of a kind: not superseded by anything, newest wins. */
export function latest(artifacts: Artifact[], kind: string): Artifact | undefined {
  const all = artifacts.filter((a) => a.kind === kind);
  const superseded = new Set(all.map((a) => a.supersedes).filter((x): x is string => !!x));
  return all.filter((a) => !superseded.has(a.id)).sort(newerFirst)[0] ?? all.sort(newerFirst)[0];
}

/** All versions of a kind, oldest first (v1, v2, …). */
export function versions(artifacts: Artifact[], kind: string): Artifact[] {
  return artifacts.filter((a) => a.kind === kind).sort((a, b) => -newerFirst(a, b));
}

/** Merge a newly-added artifact into a list (idempotent). */
export function upsertArtifact(list: Artifact[], a: Artifact): Artifact[] {
  return list.some((x) => x.id === a.id) ? list : [...list, a];
}

const SEVERITY: Record<VerdictLabel, number> = { contradicted: 3, needs_review: 2, unverifiable: 1, supported: 0 };

/** The label to show on a bullet when several verifiers disagree: the most severe wins. */
export function worstLabel(vs: Verdict[]): VerdictLabel | null {
  if (!vs.length) return null;
  return vs.reduce((w, v) => (SEVERITY[v.label] > SEVERITY[w] ? v.label : w), vs[0].label);
}

/**
 * Verdicts for each bullet of a summary, keyed by flat bullet index across sections
 * (`Claim.source.segment`, with `Claim.source.artifact === summary.id`).
 */
export function verdictsByBullet(artifacts: Artifact[], summaryId: string): Map<number, { claimText: string; verdicts: Verdict[] }[]> {
  const out = new Map<number, { claimText: string; verdicts: Verdict[] }[]>();
  const claimsArts = artifacts.filter((a) => a.kind === Kinds.claims && body(a, "claims").claims.some((c) => c.source.artifact === summaryId));
  for (const ca of claimsArts) {
    const claims = body(ca, "claims").claims.filter((c) => c.source.artifact === summaryId);
    const verdictArts = artifacts.filter((a) => a.kind === Kinds.verdicts && body(a, "verdicts").verdicts.some((v) => v.claims_artifact === ca.id));
    // Newest verdicts artifact for these claims wins.
    const va = verdictArts.sort(newerFirst)[0];
    const vlist = va ? body(va, "verdicts").verdicts : [];
    for (const c of claims) {
      const vs = vlist.filter((v) => v.claims_artifact === ca.id && v.claim_id === c.id);
      if (!vs.length) continue;
      const arr = out.get(c.source.segment) ?? [];
      arr.push({ claimText: c.text, verdicts: vs });
      out.set(c.source.segment, arr);
    }
  }
  return out;
}

/** Short model name from a provenance model ref ("ollama:qwen3.5:9b@sha" → "qwen3.5:9b"). */
export function shortModel(model: string | null | undefined): string {
  if (!model) return "—";
  return model.replace(/^(ollama|whisper|sherpa):/, "").replace(/@.*$/, "");
}

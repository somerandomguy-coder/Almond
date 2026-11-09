import { createContext } from "react";
import type { Segment } from "../gen/Segment";
import type { SpanRef } from "../gen/SpanRef";

/** Provided by the meeting view: resolve a SpanRef to its transcript segment, and jump there. */
export interface CitationCtx {
  resolve: (span: SpanRef) => Segment | undefined;
  speakerName: (id: string | null) => string;
  jump: (span: SpanRef) => void;
}
export const CitationContext = createContext<CitationCtx | null>(null);

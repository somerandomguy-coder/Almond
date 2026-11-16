import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import type { Segment } from "../gen/Segment";
import { actionItemsMarkdown, enhancedMarkdown, meetingMarkdown, summaryMarkdown, transcriptText } from "../lib/copyText";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => api?.dispose());

const seg = (idx: number, speaker: string | null, text: string, t0_ms = idx * 5000): Segment => ({
  idx,
  t0_ms,
  t1_ms: t0_ms + 4000,
  channel: "mic",
  speaker,
  lang: null,
  text,
  confidence: null,
  words: null,
});

it("formats the transcript as paragraphs, with optional names and timestamps", () => {
  const segs = [seg(0, "S1", "Chào mọi người."), seg(1, "S1", "Hôm nay chốt ngân sách."), seg(2, "S2", "Ok."), seg(3, null, " ")];
  const name = (s: string | null) => (s === "S1" ? "Lan" : s === "S2" ? "Minh" : "?");
  expect(transcriptText(segs, name)).toBe("Lan: Chào mọi người. Hôm nay chốt ngân sách.\n\nMinh: Ok.");
  expect(transcriptText(segs, name, { speakers: false, timestamps: false })).toBe("Chào mọi người. Hôm nay chốt ngân sách.\n\nOk.");
  expect(transcriptText(segs, name, { speakers: true, timestamps: true })).toBe("[0:00] Lan: Chào mọi người. Hôm nay chốt ngân sách.\n\n[0:10] Minh: Ok.");
});

it("formats summary, notes and action items as Markdown without citations", () => {
  const cites = [{ artifact: "a", segment: 1, chars: null, time_ms: null }];
  expect(
    summaryMarkdown({ lang: null, sections: [{ heading: "Quyết định", bullets: [{ text: "S1 gửi kế hoạch.", cites }] }] }, (t) => t.replace("S1", "Lan")),
  ).toBe("## Quyết định\n- Lan gửi kế hoạch.");
  expect(
    enhancedMarkdown({
      lang: null,
      blocks: [
        { origin: "ai", style: "heading", level: 0, text: "Budget", cites },
        { origin: "user", style: "bullet", level: 0, text: "200m", cites: [] },
        { origin: "ai", style: "bullet", level: 1, text: "up 15%", cites },
        { origin: "ai", style: "todo", level: 0, text: "Send plan", cites },
      ],
    } as never),
  ).toBe("## Budget\n- 200m\n  - up 15%\n- [ ] Send plan");
  expect(actionItemsMarkdown([{ text: "Send plan", owner: "Lan", due: "Friday", cites }, { text: "Demo", owner: null, due: null, cites: [] }])).toBe(
    "- [ ] Send plan — Lan, due Friday\n- [ ] Demo",
  );
  expect(meetingMarkdown({ title: "Q4", when: "Mon", sections: [{ heading: "Summary", text: "## A\n- b" }, { heading: "Notes", text: "  " }] })).toBe(
    "# Q4\nMon\n\n## Summary\n\n### A\n- b\n",
  );
});

it("Copy buttons on a meeting put clean text on the clipboard", async () => {
  const user = userEvent.setup();
  const r = renderApp("/");
  api = r.api;
  await user.click(await screen.findByText("Họp kế hoạch Q4"));
  await screen.findByDisplayValue("Họp kế hoạch Q4");

  await user.click(await screen.findByRole("button", { name: "Copy transcript" }));
  const plain = await navigator.clipboard.readText();
  expect(plain.length).toBeGreaterThan(20);
  expect(plain).not.toMatch(/\d+:\d\d/); // no timestamps by default
  expect(await screen.findByText("Copied")).toBeInTheDocument();

  await user.click(screen.getByRole("button", { name: "More copy options" }));
  await user.click(screen.getByRole("menuitem", { name: "With names and timestamps" }));
  expect(await navigator.clipboard.readText()).toMatch(/^\[\d+:\d\d\] /);

  await user.click(screen.getByRole("button", { name: "Copy all" }));
  const all = await navigator.clipboard.readText();
  expect(all).toMatch(/^# Họp kế hoạch Q4\n/);
  expect(all).toContain("## Transcript");
});

import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => api?.dispose());

async function openQ4() {
  const r = renderApp("/");
  api = r.api;
  const q4 = api.meetings.find((m) => m.title === "Họp kế hoạch Q4")!;
  await userEvent.click(await screen.findByText("Họp kế hoạch Q4"));
  await screen.findByDisplayValue("Họp kế hoạch Q4");
  return { ...r, q4 };
}

describe("meeting view", () => {
  it("shows enhanced notes with AI blocks styled apart from the user's own text", async () => {
    await openQ4();
    const notes = await screen.findByTestId("enhanced-notes");
    const user = within(notes).getByText("ngân sách mkt");
    const ai = within(notes).getByText(/Marketing: 200 triệu đồng/);
    expect(user.closest("[data-origin]")).toHaveAttribute("data-origin", "user");
    expect(ai.closest("[data-origin]")).toHaveAttribute("data-origin", "ai");
    expect(ai.closest("[data-origin]")!.className).toMatch(/text-ai/);
  });

  it("citation chip jumps to the transcript segment and audio time", async () => {
    await openQ4();
    await userEvent.click(screen.getByRole("tab", { name: "Summary" }));
    const summary = await screen.findByTestId("summary-current");
    const bullet = within(summary).getByText(/Ngân sách marketing Q4 là 200 triệu/).closest("li")!;
    // hover shows the quoted words
    const chip = within(bullet).getByRole("button", { name: /Jump to transcript 0:06/ });
    await userEvent.hover(chip);
    expect(await screen.findByRole("tooltip")).toHaveTextContent("Ngân sách marketing là 200 triệu đồng, tăng 15% so với Q3.");
    await userEvent.click(chip);
    const transcript = screen.getByTestId("transcript");
    await waitFor(() => {
      const focused = transcript.querySelector('[aria-current="true"]');
      expect(focused).toHaveAttribute("data-segment", "1");
    });
    expect(screen.getByTestId("audio-position")).toHaveTextContent("0:06");
  });

  it("version switcher compares reruns and shows verdict badges with evidence", async () => {
    await openQ4();
    await userEvent.click(screen.getByRole("tab", { name: "Summary" }));
    await screen.findByTestId("summary-current");
    // Latest (v2) has no contradicted bullet; v1 (gemma on the Fedora laptop) does.
    expect(within(screen.getByTestId("summary-current")).queryByText("Contradicted")).toBeNull();
    const compare = screen.getByRole("combobox", { name: "Compare with" });
    const v1 = within(compare).getByRole("option", { name: /v1 · gemma4:e4b · Fedora laptop/ }) as HTMLOptionElement;
    await userEvent.selectOptions(compare, v1.value);
    const other = await screen.findByTestId("summary-compare");
    const bad = within(other).getByRole("button", { name: "Verification: Contradicted" });
    expect(bad.closest("li")).toHaveAttribute("data-verdict", "contradicted");
    await userEvent.click(bad);
    const pop = await screen.findByRole("dialog", { name: "Verification" });
    expect(pop).toHaveTextContent("verify.numbers");
    expect(pop).toHaveTextContent("Transcript says 200 triệu đồng, not 300.");
    expect(within(other).getByRole("button", { name: "Verification: Unverifiable" })).toBeInTheDocument();
    expect(within(other).getByRole("button", { name: "Verification: Needs review" })).toBeInTheDocument();
  });

  it("records with live transcript, stops, then enhances notes", async () => {
    const r = renderApp("/");
    api = r.api;
    await userEvent.click(await screen.findByRole("button", { name: /New note/ }));
    // Live captions stream while recording.
    await screen.findByText("Live transcript", {}, { timeout: 6000 });
    await screen.findByText("Ok mọi người, mình bắt đầu standup nhé.", {}, { timeout: 5000 });
    const editor = screen.getByRole("textbox", { name: /Write rough notes/ });
    expect(editor).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Stop" }));
    // Pipeline runs after stop: transcript, summary, verdicts appear.
    await waitFor(() => expect(r.api.artifacts.some((a) => a.kind === "summary" && a.meeting_id === r.api.meetings.at(-1)!.id)).toBe(true), { timeout: 8000 });
    const enhance = await screen.findByRole("button", { name: "Enhance notes" });
    await waitFor(() => expect(enhance).toBeEnabled());
    await userEvent.click(enhance);
    await screen.findByTestId("enhanced-notes", {}, { timeout: 5000 });
    expect(screen.getAllByText(/Tóm tắt thêm/).length).toBeGreaterThan(0);
  });

  it("executes a proposed action", async () => {
    await openQ4();
    await userEvent.click(screen.getByRole("tab", { name: "Actions" }));
    await screen.findByText("Draft follow-up email to Lan, David");
    const execs = screen.getAllByRole("button", { name: "Execute" });
    await userEvent.click(execs[0]);
    await screen.findByText(/Draft saved in Gmail/);
  });

  it("reminder toast starts notes for the event", async () => {
    const r = renderApp("/");
    api = r.api;
    await screen.findByText("Coming up");
    act(() => r.api.fireReminder());
    const toast = await screen.findByRole("status");
    expect(toast).toHaveTextContent("Standup nhóm Almond");
    await userEvent.click(within(toast).getByRole("button", { name: /Start notes/ }));
    await screen.findByDisplayValue("Standup nhóm Almond", {}, { timeout: 6000 });
    await screen.findByText("Live transcript", {}, { timeout: 6000 });
    await act(() => r.api.call("recording.stop", {}));
  });
});

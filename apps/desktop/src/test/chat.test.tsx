import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { blocks } from "../components/chat/ChatAnswer";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => {
  api?.dispose();
  localStorage.clear();
});

async function home(lang: "en" | "vi" = "en") {
  const r = renderApp("/", { lang });
  api = r.api;
  const box = await screen.findByTestId("ask-almond");
  return { ...r, box };
}

/** Wait until the answer has finished streaming, then return its element containing `text`. */
async function answered(box: HTMLElement, text: RegExp): Promise<HTMLElement> {
  await waitFor(() => expect(within(box).queryByTestId("chat-streaming")).toBeNull(), { timeout: 5000 });
  return within(box).findByText(text, {}, { timeout: 5000 });
}

describe("Ask Almond floating bar", () => {
  it("streams a cited answer; a source chip opens the meeting at that transcript line", async () => {
    const { box } = await home();
    // Folded: just the bar with its prompt.
    expect(within(box).queryByRole("region", { name: "Ask Almond" })).toBeNull();
    const input = within(box).getByRole("textbox", { name: "Ask about your notes and calendar…" });
    await userEvent.type(input, "What did we decide about the marketing budget?{Enter}");
    // The question shows at once, then the answer streams in.
    expect(within(box).getByText("What did we decide about the marketing budget?")).toBeInTheDocument();
    expect(await within(box).findByTestId("chat-streaming")).toBeInTheDocument();
    const answer = await answered(box, /Ngân sách marketing là 200 triệu đồng/);
    // Inline [1] chip + the labelled source list.
    const chip = within(answer.closest("li")!).getByRole("button", { name: /^Source 1: Họp kế hoạch Q4 · 0:06/ });
    expect(within(box).getByRole("list", { name: "Sources" })).toHaveTextContent("Transcript");
    await userEvent.click(chip);
    await screen.findByDisplayValue("Họp kế hoạch Q4");
    await waitFor(() => {
      const focused = screen.getByTestId("transcript").querySelector('[aria-current="true"]');
      expect(focused).toHaveAttribute("data-segment", "1");
    });
  });

  it("folds with Close, keeps threads under Recent chats", async () => {
    const { box } = await home();
    await userEvent.type(within(box).getByRole("textbox"), "What do I have today?{Enter}");
    const ans = await answered(box, /things coming up/);
    expect(within(ans.closest("li")!).getByText("Standup nhóm Almond")).toBeInTheDocument();
    expect(api!.chats.size).toBe(1);
    await userEvent.click(within(box).getByRole("button", { name: "Close" }));
    expect(within(box).queryByRole("region", { name: "Ask Almond" })).toBeNull();
    // Focusing the bar again reopens the conversation.
    await userEvent.click(within(box).getByRole("textbox"));
    expect(await within(box).findByText(/things coming up/)).toBeInTheDocument();
    // New chat, then reopen the saved thread from Recent chats.
    await userEvent.click(within(box).getByRole("button", { name: "New chat" }));
    await userEvent.click(within(box).getByRole("button", { name: "Recent chats" }));
    await userEvent.click(await within(box).findByRole("button", { name: "What do I have today?" }));
    expect(await within(box).findByText(/things coming up/)).toBeInTheDocument();
  });

  it("says clearly when Ollama isn't running", async () => {
    const { box } = await home();
    api!.llmDown = true;
    await userEvent.type(within(box).getByRole("textbox"), "What do I have today?{Enter}");
    const alert = await within(box).findByRole("alert");
    expect(alert).toHaveTextContent("Almond's AI model isn't running. Start Ollama");
    // The question is back in the box to retry.
    expect(within(box).getByRole("textbox")).toHaveValue("What do I have today?");
    expect(api!.chats.size).toBe(0);
  });

  it("speaks Vietnamese", async () => {
    const { box } = await home("vi");
    await userEvent.type(within(box).getByRole("textbox", { name: "Hỏi về ghi chú và lịch của bạn…" }), "Hôm nay mình có gì?{Enter}");
    expect(await answered(box, /lịch sắp tới/)).toBeInTheDocument();
  });

  it("on a meeting asks about that meeting, and its chips jump within the transcript", async () => {
    const r = renderApp("/");
    api = r.api;
    await userEvent.click(await screen.findByText("Họp kế hoạch Q4"));
    await screen.findByDisplayValue("Họp kế hoạch Q4");
    const box = await screen.findByTestId("ask-almond");
    await userEvent.type(within(box).getByRole("textbox", { name: "Ask about this meeting…" }), "Who is the tester for QA?{Enter}");
    const ans = await answered(box, /đội QA hơi thiếu người/);
    expect(api.chats.values().next().value!.scope).toEqual({ type: "meeting", meeting_id: api.meetings[0].id });
    await userEvent.click(within(ans.closest("li")!).getByRole("button", { name: /^Source \d+: Họp kế hoạch Q4 · 0:45/ }));
    await waitFor(() => {
      const focused = screen.getByTestId("transcript").querySelector('[aria-current="true"]');
      expect(focused).toHaveAttribute("data-segment", "7");
    });
  });
});

describe("chat markdown", () => {
  it("parses paragraphs, lists and headings", () => {
    expect(blocks("Hi [1].\n\n- a [2]\n- b\n1. x\n## H")).toEqual([
      { type: "p", text: "Hi [1]." },
      { type: "ul", items: ["a [2]", "b"] },
      { type: "ol", items: ["x"] },
      { type: "h", text: "H" },
    ]);
  });
});

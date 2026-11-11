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

describe("Ask Almond on Home", () => {
  it("streams a cited answer; a source chip opens the meeting at that transcript line", async () => {
    const { box } = await home();
    const input = within(box).getByRole("textbox", { name: /Ask about your meetings/ });
    await userEvent.type(input, "What did we decide about the marketing budget?{Enter}");
    // The question shows at once, then the answer streams in.
    expect(within(box).getByText("What did we decide about the marketing budget?")).toBeInTheDocument();
    expect(await within(box).findByTestId("chat-streaming")).toBeInTheDocument();
    const answer = await answered(box, /Ngân sách marketing là 200 triệu đồng/);
    await waitFor(() => expect(within(box).queryByTestId("chat-streaming")).toBeNull());
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

  it("answers a suggestion, keeps the thread and lists it under recent chats", async () => {
    const { box } = await home();
    await userEvent.click(within(box).getByRole("button", { name: "What do I have today?" }));
    const ans = await answered(box, /things coming up/);
    expect(within(ans.closest("li")!).getByText("Standup nhóm Almond")).toBeInTheDocument();
    expect(api!.chats.size).toBe(1);
    // New chat → the old thread is listed; opening it restores the conversation.
    await userEvent.click(within(box).getByRole("button", { name: "New chat" }));
    expect(within(box).getByText("Recent chats")).toBeInTheDocument();
    // [0] is the suggestion chip, [1] the saved thread.
    await userEvent.click(within(box).getAllByRole("button", { name: "What do I have today?" })[0]);
    await answered(box, /things coming up/);
    await userEvent.click(within(box).getByRole("button", { name: "New chat" }));
    const recent = within(box).getAllByRole("button", { name: "What do I have today?" });
    // One suggestion chip + two saved threads with that title.
    expect(recent).toHaveLength(3);
    await userEvent.click(recent[1]);
    expect(await within(box).findByText(/things coming up/)).toBeInTheDocument();
    // "Decided about…" fills the box instead of asking.
    await userEvent.click(within(box).getByRole("button", { name: "New chat" }));
    await userEvent.click(within(box).getByRole("button", { name: "What did we decide about…" }));
    expect(within(box).getByRole("textbox")).toHaveValue("What did we decide about ");
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
    expect(within(box).getByRole("heading", { name: "Hỏi Almond" })).toBeInTheDocument();
    await userEvent.click(within(box).getByRole("button", { name: "Hôm nay mình có gì?" }));
    expect(await answered(box, /lịch sắp tới/)).toBeInTheDocument();
  });
});

describe("Ask about this meeting", () => {
  it("answers in the meeting tab and its chips jump within the transcript", async () => {
    const r = renderApp("/");
    api = r.api;
    await userEvent.click(await screen.findByText("Họp kế hoạch Q4"));
    await screen.findByDisplayValue("Họp kế hoạch Q4");
    await userEvent.click(screen.getByRole("tab", { name: "Ask" }));
    const box = await screen.findByTestId("ask-almond");
    expect(within(box).getByRole("heading", { name: "Ask about this meeting" })).toBeInTheDocument();
    await userEvent.type(within(box).getByRole("textbox"), "Who is the tester for QA?{Enter}");
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

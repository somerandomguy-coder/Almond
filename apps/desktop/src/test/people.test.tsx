import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { nameSpeakersInText } from "../lib/people";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => api?.dispose());

async function openQ4() {
  const r = renderApp("/");
  api = r.api;
  await userEvent.click(await screen.findByText("Họp kế hoạch Q4"));
  await screen.findByDisplayValue("Họp kế hoạch Q4");
  return r;
}

const transcript = () => screen.getByTestId("transcript");

describe("speaker labels", () => {
  it("shows a voice suggestion dimmed and confirms it with the tick", async () => {
    await openQ4();
    const chips = await within(transcript()).findAllByRole("button", { name: "David Kim" });
    expect(chips[0]).toHaveAttribute("data-suggested", "true");
    // "me" and Lan are confirmed
    expect(within(transcript()).getAllByRole("button", { name: "Trần Lan" })[0]).not.toHaveAttribute("data-suggested");
    expect(within(transcript()).getAllByRole("button", { name: "Nam Nguyễn" }).length).toBeGreaterThan(0);

    await userEvent.click(within(transcript()).getAllByRole("button", { name: "Confirm David Kim" })[0]);
    await waitFor(() => expect(within(transcript()).queryAllByRole("button", { name: "Confirm David Kim" })).toHaveLength(0));
    expect(within(transcript()).getAllByRole("button", { name: "David Kim" })[0]).not.toHaveAttribute("data-suggested");
    const david = api!.people.people.find((p) => p.name === "David Kim")!;
    expect(david.voiceprints).toHaveLength(1);
  });

  it("names a speaker from the popover: new person, then 'this is me'", async () => {
    await openQ4();
    await userEvent.click((await within(transcript()).findAllByRole("button", { name: "Trần Lan" }))[0]);
    const pop = await screen.findByRole("dialog", { name: "Who is this?" });
    // existing people are offered
    expect(within(pop).getByRole("button", { name: "David Kim" })).toBeInTheDocument();
    await userEvent.type(within(pop).getByRole("textbox", { name: "Name…" }), "Lê Minh");
    await userEvent.click(within(pop).getByRole("button", { name: "Add “Lê Minh”" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await waitFor(() => expect(within(transcript()).getAllByRole("button", { name: "Lê Minh" }).length).toBe(3));
    expect(within(transcript()).queryAllByRole("button", { name: "Trần Lan" })).toHaveLength(0);

    // S2 → "this is me"
    await userEvent.click(within(transcript()).getAllByRole("button", { name: "David Kim" })[0]);
    const pop2 = await screen.findByRole("dialog", { name: "Who is this?" });
    await userEvent.click(within(pop2).getByRole("button", { name: "This is me" }));
    await waitFor(() => expect(within(transcript()).queryAllByRole("button", { name: "David Kim" })).toHaveLength(0));

    // The new person is in the catalog with this meeting.
    await userEvent.click(screen.getAllByRole("link", { name: "People" })[0]);
    const list = await screen.findByTestId("people-list");
    const row = within(list).getByText("Lê Minh").closest("a")!;
    expect(row).toHaveTextContent("1 meeting");
    await userEvent.click(row);
    await screen.findByDisplayValue("Lê Minh");
    expect(screen.getByText("Họp kế hoạch Q4")).toBeInTheDocument();
    expect(screen.getByTestId("talk-time")).toHaveTextContent(/s|min/);
  });

  it("replaces speaker ids in AI text with names", () => {
    const names = new Map([
      ["remote:S1", "Lan"],
      ["me", "Nam"],
    ]);
    expect(nameSpeakersInText("S1 will send the plan; remote:S1 agreed. S12 stays.", names)).toBe("Lan will send the plan; Lan agreed. S12 stays.");
  });
});

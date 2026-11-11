import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { parseBlocks } from "../components/recipes/Markdown";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => {
  api?.dispose();
  vi.restoreAllMocks();
});

const card = (name: string) => screen.getByRole("listitem", { name });

describe("cookbook", () => {
  it("lists the built-ins, creates a recipe and copies an edited built-in", async () => {
    const r = renderApp("/cookbook");
    api = r.api;
    expect(await screen.findByRole("heading", { name: "Cookbook" })).toBeInTheDocument();
    for (const n of ["Teach it back to me", "Questions I forgot to ask", "How did I communicate?", "Decisions & owners", "Follow-up email draft"]) {
      expect(within(card(n)).getByText("Built-in")).toBeInTheDocument();
    }
    // Built-ins can be hidden, not deleted.
    expect(within(card("Teach it back to me")).queryByRole("button", { name: "Delete" })).toBeNull();

    // New recipe
    await userEvent.click(screen.getByRole("button", { name: "New recipe" }));
    const form = screen.getByRole("form", { name: "New recipe" });
    await userEvent.type(within(form).getByLabelText("Name"), "Risk radar");
    await userEvent.type(within(form).getByLabelText("Icon (emoji)"), "🛰️");
    await userEvent.type(within(form).getByLabelText("Instructions for the AI"), "List every risk with an owner.");
    await userEvent.click(within(form).getByRole("switch", { name: "Run automatically on every new meeting" }));
    await userEvent.click(within(form).getByRole("button", { name: "Save" }));
    const mine = await screen.findByRole("listitem", { name: "Risk radar" });
    expect(within(mine).getByRole("button", { name: "Auto-run", pressed: true })).toBeInTheDocument();
    expect(within(mine).getAllByText("Auto-run")).toHaveLength(2); // badge + toggle
    const saved = api.recipes.find((x) => x.name === "Risk radar")!;
    expect(saved).toMatchObject({ builtin: false, auto_run: true, icon: "🛰️", prompt: "List every risk with an owner." });

    // Editing a built-in's text saves a copy; the built-in stays.
    await userEvent.click(within(card("Decisions & owners")).getByRole("button", { name: "Edit" }));
    const edit = screen.getByRole("form", { name: "Edit recipe" });
    expect(within(edit).getByText(/creates your own copy/)).toBeInTheDocument();
    const name = within(edit).getByLabelText("Name");
    await userEvent.clear(name);
    await userEvent.type(name, "Decisions (short)");
    await userEvent.click(within(edit).getByRole("button", { name: "Save" }));
    await screen.findByRole("listitem", { name: "Decisions (short)" });
    expect(card("Decisions & owners")).toBeInTheDocument();
    expect(api.recipes.find((x) => x.name === "Decisions (short)")).toMatchObject({ builtin: false });
    expect(api.recipes.find((x) => x.id === "builtin:decisions")!.name).toBe("Decisions & owners");

    // Delete the user recipe.
    vi.spyOn(window, "confirm").mockReturnValue(true);
    await userEvent.click(within(card("Risk radar")).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "Risk radar" })).toBeNull());
  });

  it("hides a built-in and shows it again", async () => {
    const r = renderApp("/cookbook");
    api = r.api;
    await screen.findByRole("listitem", { name: "Teach it back to me" });
    await userEvent.click(within(card("Teach it back to me")).getByRole("button", { name: "Hide" }));
    await waitFor(() => expect(screen.queryByRole("listitem", { name: "Teach it back to me" })).toBeNull());
    await userEvent.click(screen.getByLabelText(/Show hidden recipes/));
    const hidden = await screen.findByRole("listitem", { name: "Teach it back to me" });
    expect(within(hidden).getByText("Hidden")).toBeInTheDocument();
    await userEvent.click(within(hidden).getByRole("button", { name: "Show" }));
    await waitFor(() => expect(api!.recipes.find((x) => x.id === "builtin:teach_back")!.hidden).toBe(false));
  });

  it("runs a recipe on a meeting from the cookbook and shows the result in the meeting", async () => {
    const r = renderApp("/cookbook");
    api = r.api;
    await screen.findByRole("listitem", { name: "How did I communicate?" });
    const c = card("How did I communicate?");
    await userEvent.click(within(c).getByRole("button", { name: "Run on…" }));
    const select = within(c).getByRole("combobox", { name: "Meeting to run “How did I communicate?” on" });
    const q4 = api.meetings.find((m) => m.title === "Họp kế hoạch Q4")!;
    await userEvent.selectOptions(select, q4.id);
    await userEvent.click(within(c).getByRole("button", { name: "Run" }));
    expect(await within(c).findByRole("status")).toHaveTextContent("Running “How did I communicate?” on “Họp kế hoạch Q4”.");
    await userEvent.click(within(c).getByRole("link", { name: /Open meeting/ }));

    await screen.findByDisplayValue("Họp kế hoạch Q4");
    await userEvent.click(screen.getByRole("tab", { name: "Recipes" }));
    const out = await screen.findByRole("region", { name: "How did I communicate?" }, { timeout: 3000 });
    // Markdown table + a citation chip that jumps to the transcript.
    expect(within(out).getByRole("table")).toBeInTheDocument();
    const chip = within(out).getAllByRole("button", { name: /Jump to transcript/ })[0];
    await userEvent.click(chip);
    await waitFor(() => expect(screen.getByTestId("transcript").querySelector('[aria-current="true"]')).toHaveAttribute("data-segment", "1"));
  });

  it("runs and reruns a recipe from the meeting's Recipes tab", async () => {
    const r = renderApp("/");
    api = r.api;
    await userEvent.click(await screen.findByText("Họp kế hoạch Q4"));
    await screen.findByDisplayValue("Họp kế hoạch Q4");
    await userEvent.click(screen.getByRole("tab", { name: "Recipes" }));
    const panel = await screen.findByTestId("recipes-panel");
    expect(within(panel).getByText(/No recipe results yet/)).toBeInTheDocument();
    await userEvent.selectOptions(within(panel).getByRole("combobox", { name: "Choose a recipe" }), "builtin:decisions");
    await userEvent.type(within(panel).getByRole("textbox", { name: "Extra instruction (optional)" }), "hiring only");
    await userEvent.click(within(panel).getByRole("button", { name: "Run a recipe" }));
    const out = await screen.findByRole("region", { name: "Decisions & owners" }, { timeout: 3000 });
    expect(within(out).getByText(/Focus: hiring only/)).toBeInTheDocument();
    const first = api.artifacts.filter((a) => a.kind === "recipe_output");
    expect(first).toHaveLength(1);

    await userEvent.click(within(out).getByRole("button", { name: "Run again" }));
    await waitFor(() => expect(api!.artifacts.filter((a) => a.kind === "recipe_output")).toHaveLength(2), { timeout: 3000 });
    const second = api.artifacts.filter((a) => a.kind === "recipe_output")[1];
    expect(second.supersedes).toBe(first[0].id);
    // Still one result per recipe on screen.
    await waitFor(() => expect(screen.getAllByRole("region", { name: "Decisions & owners" })).toHaveLength(1));
  });

  it("shows built-in names in Vietnamese", async () => {
    const r = renderApp("/cookbook", { lang: "vi" });
    api = r.api;
    expect(await screen.findByRole("listitem", { name: "Tôi giao tiếp thế nào?" })).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Sổ công thức" })).toBeInTheDocument();
  });
});

describe("recipe markdown", () => {
  it("parses headings, nested lists, tables and paragraphs", () => {
    const b = parseBlocks("## Title\n- a [1]\n  - b\n1. one\n\n| A | B |\n|---|---|\n| x | [2] |\n\nplain\ntext");
    expect(b.map((x) => x.t)).toEqual(["h", "list", "list", "table", "p"]);
    expect(b[1]).toMatchObject({ t: "list", items: [{ indent: 0, text: "a [1]" }, { indent: 2, text: "b" }] });
    expect(b[3]).toMatchObject({ head: ["A", "B"], rows: [["x", "[2]"]] });
    expect(b[4]).toMatchObject({ text: "plain text" });
  });
});

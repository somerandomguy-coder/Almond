import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { renderApp } from "./render";

let api: MockApi | null = null;
beforeEach(() => {
  vi.spyOn(window, "confirm").mockReturnValue(true);
});
afterEach(() => {
  api?.dispose();
  vi.restoreAllMocks();
});

it("deletes a meeting's audio and keeps its text; starred audio can't be deleted", async () => {
  const r = renderApp("/");
  api = r.api;
  await userEvent.click(await screen.findByText("Họp kế hoạch Q4"));
  await screen.findByDisplayValue("Họp kế hoạch Q4");
  const id = api.meetings[0].id;

  // ⭐ keep → delete is disabled; unstar → delete.
  await userEvent.click(await screen.findByRole("button", { name: /Keep audio/ }));
  expect(api.audioState.get(id)?.keep).toBe(true);
  expect(screen.getByRole("button", { name: /Delete audio/ })).toBeDisabled();
  await userEvent.click(screen.getByRole("button", { name: /Keeping audio/ }));
  await userEvent.click(screen.getByRole("button", { name: /Delete audio/ }));
  expect(await screen.findByTestId("audio-deleted")).toHaveTextContent("Audio deleted on");
  expect(api.audioState.get(id)?.deleted_at).toBeTypeOf("number");
  // the transcript is still there
  expect(screen.getByTestId("transcript")).toBeInTheDocument();
});

it("Settings → Storage: retention and bulk delete", async () => {
  const r = renderApp("/settings");
  api = r.api;
  const select = await screen.findByRole("combobox", { name: "Delete recordings automatically" });
  await userEvent.selectOptions(select, "30");
  await waitFor(async () => expect(await api!.call("settings.get", { key: "storage.audio_retention_days" })).toBe(30));

  const list = await screen.findByRole("list", { name: "Recordings on this device" });
  const boxes = within(list).getAllByRole("checkbox").filter((b) => !(b as HTMLInputElement).disabled);
  expect(boxes.length).toBeGreaterThan(0);
  await userEvent.click(boxes[0]);
  await userEvent.click(screen.getByRole("button", { name: "Delete audio (1)" }));
  expect(await screen.findByText(/Deleted 1 recording, freed/)).toBeInTheDocument();
});

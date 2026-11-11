import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => api?.dispose());

it("tells the user once that the database was repaired", async () => {
  const r = renderApp("/", {
    storeRecovery: { reason: "malformed", backup_dir: "C:\data\corrupt-1", events_recovered: 213, events_lost: 24, tables: {} },
  });
  api = r.api;
  expect(await screen.findByText("Almond repaired its database")).toBeInTheDocument();
  expect(screen.getByText(/213 changes kept, 24 unreadable.*corrupt-1/)).toBeInTheDocument();
  await userEvent.click(screen.getByRole("button", { name: "OK" }));
  await waitFor(() => expect(screen.queryByText("Almond repaired its database")).toBeNull());
  expect(api.storeRecovery).toBeNull();
});

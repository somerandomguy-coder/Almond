import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => api?.dispose());

describe("import audio/video", () => {
  it("picks a file, shows progress, then opens the imported meeting", async () => {
    const r = renderApp("/");
    api = r.api;
    const before = api.meetings.length;
    const progress: number[] = [];
    api.on("import.progress", (p) => p.progress != null && progress.push(p.progress));

    await userEvent.click(await screen.findByRole("button", { name: "Import audio/video" }));
    // progress card while importing
    expect(await screen.findByTestId("import-card")).toHaveTextContent("Phỏng vấn khách hàng.m4a");
    expect(screen.getByRole("button", { name: "Import audio/video" })).toBeDisabled();

    // then the meeting view of the new meeting
    expect(await screen.findByDisplayValue("Phỏng vấn khách hàng")).toBeInTheDocument();
    expect(api.meetings).toHaveLength(before + 1);
    const m = api.meetings[api.meetings.length - 1];
    expect(m.kind).toEqual({ type: "imported" });
    expect(m.audio[0].channel).toBe("mixed");
    expect(progress[progress.length - 1]).toBe(1);
  });

  it("can be cancelled while importing", async () => {
    const r = renderApp("/", { speed: 1 });
    api = r.api;
    const before = api.meetings.length;
    await userEvent.click(await screen.findByRole("button", { name: "Import audio/video" }));
    const cancel = await screen.findByRole("button", { name: "Cancel" });
    await waitFor(() => expect(cancel).toBeEnabled());
    await userEvent.click(cancel);
    await waitFor(() => expect(screen.queryByTestId("import-card")).toBeNull(), { timeout: 3000 });
    expect(api.meetings).toHaveLength(before);
    expect(screen.getByRole("button", { name: "Import audio/video" })).toBeEnabled();
  });

  it("does nothing when the file dialog is closed, and shows import errors", async () => {
    const r = renderApp("/");
    api = r.api;
    api.pickedFile = null;
    const button = await screen.findByRole("button", { name: "Import audio/video" });
    await userEvent.click(button);
    expect(screen.queryByTestId("import-card")).toBeNull();

    api.pickedFile = "/home/me/notes.pdf";
    await userEvent.click(button);
    expect(await screen.findByText("Import failed")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("not an audio or video file");
    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByTestId("import-card")).toBeNull();
  });

  it("is translated", async () => {
    const r = renderApp("/", { lang: "vi" });
    api = r.api;
    expect(await screen.findByRole("button", { name: "Nhập âm thanh/video" })).toBeInTheDocument();
  });
});

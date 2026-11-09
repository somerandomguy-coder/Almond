import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";
import type { MockApi } from "../api/mock/MockApi";
import { detectLanguage } from "../i18n";
import { renderApp } from "./render";

let api: MockApi | null = null;
afterEach(() => {
  api?.dispose();
  localStorage.clear();
});

describe("devices & sync", () => {
  it("Sync now shows per-peer progress, then reports, and brings the phone's meeting over", async () => {
    const r = renderApp("/devices");
    api = r.api;
    await screen.findByText("Fedora laptop");
    expect(screen.getByText("Galaxy A26")).toBeInTheDocument();
    const before = r.api.meetings.length;
    await userEvent.click(screen.getByRole("button", { name: "Sync now" }));
    expect(await screen.findByRole("button", { name: "Syncing…" })).toBeDisabled();
    await screen.findAllByRole("progressbar");
    await screen.findByRole("button", { name: "Sync now" }, { timeout: 15000 });
    const fedora = screen.getByTestId("peer-Fedora laptop").closest("li")!;
    expect(within(fedora).getByText(/events ·/)).toBeInTheDocument();
    expect(r.api.meetings.length).toBe(before + 1);
  });

  it("pairs with a six-word code", async () => {
    const r = renderApp("/devices");
    api = r.api;
    const input = await screen.findByLabelText("Enter a code");
    await userEvent.type(input, "only three words");
    await userEvent.click(screen.getByRole("button", { name: "Pair" }));
    expect(await screen.findByText("Enter exactly six words.")).toBeInTheDocument();
    await userEvent.clear(input);
    await userEvent.type(input, "almond river lotus basket cloud pepper");
    await userEvent.click(screen.getByRole("button", { name: "Pair" }));
    expect(await screen.findByText("Paired with Office desktop.")).toBeInTheDocument();
    await screen.findByText("Office desktop");
  });

  it("shows a QR + six words for the other device", async () => {
    const r = renderApp("/devices");
    api = r.api;
    await userEvent.click(await screen.findByRole("button", { name: "Show pairing code" }));
    const words = await screen.findByRole("list", { name: "pairing words" });
    expect(within(words).getAllByRole("listitem")).toHaveLength(6);
  });

  it("auto-sync toggle persists to settings", async () => {
    const r = renderApp("/devices");
    api = r.api;
    const sw = await screen.findByRole("switch", { name: "Auto-sync while the app is open" });
    expect(sw).toHaveAttribute("aria-checked", "false");
    await userEvent.click(sw);
    await waitFor(async () => expect(await r.api.call("settings.get", { key: "sync.auto_while_open" })).toBe(true));
  });
});

describe("i18n", () => {
  it("defaults to Vietnamese when the OS locale is vi", () => {
    expect(detectLanguage(["vi-VN", "en-US"])).toBe("vi");
    expect(detectLanguage(["en-US"])).toBe("en");
    expect(detectLanguage(["fr-FR"])).toBe("en");
  });

  it("switches the whole UI between English and Vietnamese", async () => {
    const r = renderApp("/settings", { lang: "en" });
    api = r.api;
    expect((await screen.findAllByText("Settings")).length).toBeGreaterThan(0);
    await userEvent.selectOptions(screen.getByLabelText("Language"), "vi");
    expect((await screen.findAllByText("Cài đặt")).length).toBeGreaterThan(0);
    expect(screen.getAllByText("Thiết bị & Đồng bộ").length).toBeGreaterThan(0);
    expect(screen.getByRole("button", { name: /Thêm tài khoản Google/ })).toBeInTheDocument();
    expect(localStorage.getItem("almond.lang")).toBe("vi");
    expect(detectLanguage(["en-US"])).toBe("vi"); // saved choice wins
    await userEvent.selectOptions(screen.getByLabelText("Ngôn ngữ"), "en");
    expect((await screen.findAllByText("Devices & Sync")).length).toBeGreaterThan(0);
  });

  it("only shows 'Import phone calendars' on Android", async () => {
    let r = renderApp("/settings", { lang: "en", platform: "windows" });
    await screen.findByRole("button", { name: /Add Google account/ });
    expect(screen.queryByRole("button", { name: /Import phone calendars/ })).toBeNull();
    r.api.dispose();
    r.unmount();
    r = renderApp("/settings", { lang: "en", platform: "android" });
    api = r.api;
    await userEvent.click(await screen.findByRole("button", { name: /Import phone calendars/ }));
    await screen.findByText("Galaxy A26 · Samsung Calendar");
  });
});

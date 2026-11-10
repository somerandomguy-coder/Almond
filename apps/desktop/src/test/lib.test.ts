import { describe, expect, it } from "vitest";
import { MockApi } from "../api/mock/MockApi";
import { commandName } from "../api/tauri";
import { latest, verdictsByBullet, versions, worstLabel } from "../lib/artifacts";
import { fold, snippet } from "../lib/text";

describe("text", () => {
  it("folds Vietnamese diacritics for search", () => {
    expect(fold("Họp kế hoạch")).toBe("hop ke hoach");
    expect(fold("đồng")).toBe("dong");
    expect(snippet("Ngân sách marketing là 200 triệu đồng.", "trieu dong")).toBe("Ngân sách marketing là 200 «triệu đồng».");
  });
});

describe("api naming", () => {
  it("maps methods to Tauri command names", () => {
    expect(commandName("meetings.list")).toBe("meetings_list");
    expect(commandName("notes.applyUpdate")).toBe("notes_applyUpdate");
  });
});

describe("artifacts", () => {
  const api = new MockApi({ reminderAfterMs: null });
  const q4 = api.meetings.find((m) => m.title === "Họp kế hoạch Q4")!;
  const arts = api.artifacts.filter((a) => a.meeting_id === q4.id);

  it("picks the non-superseded summary as latest and orders versions", () => {
    const all = versions(arts, "summary");
    expect(all).toHaveLength(2);
    expect(latest(arts, "summary")!.id).toBe(all[1].id);
    expect(all[1].supersedes).toBe(all[0].id);
  });

  it("maps verdicts onto summary bullets, worst label wins", () => {
    const v1 = versions(arts, "summary")[0];
    const map = verdictsByBullet(arts, v1.id);
    expect(worstLabel(map.get(0)!.flatMap((c) => c.verdicts))).toBe("contradicted");
    expect(worstLabel(map.get(1)!.flatMap((c) => c.verdicts))).toBe("supported");
    expect(worstLabel(map.get(6)!.flatMap((c) => c.verdicts))).toBe("unverifiable");
  });

  it("search finds text without diacritics", async () => {
    const hits = await api.call("search.query", { text: "ngan sach" });
    expect(hits.length).toBeGreaterThan(0);
    expect(hits[0].snippet).toContain("«");
    api.dispose();
  });
});

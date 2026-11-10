import type { AlmondApi } from "../api/types";
import type { AudioChannel } from "../gen/AudioChannel";
import type { CalendarEvent } from "../gen/CalendarEvent";
import type { MeetingKind } from "../gen/MeetingKind";

export function channelsFor(platform: string): AudioChannel[] {
  // Phone = in-person recorder (mic only); desktops capture mic + system audio for online calls.
  return platform === "android" ? ["mic"] : ["mic", "system"];
}

/** Create a meeting (optionally linked to a calendar event) and start recording with live captions. */
export async function startNotes(api: AlmondApi, opts: { event?: CalendarEvent | null; meetingId?: string | null; platform: string }): Promise<string> {
  const ev = opts.event ?? null;
  let meetingId = opts.meetingId ?? null;
  if (!meetingId) {
    const kind: MeetingKind = ev?.conference_url || opts.platform !== "android" ? { type: "online", app: ev?.conference_app ?? null } : { type: "in_person" };
    const m = await api.call("meetings.create", {
      title: ev?.title ?? null,
      kind,
      calendar_event: ev ? { source: ev.source, event_id: ev.event_id } : null,
    });
    meetingId = m.id;
  }
  await api.call("recording.start", {
    meeting_id: meetingId,
    create: null,
    channels: channelsFor(opts.platform),
    mic_device: null,
    system_device: null,
    live_transcript: true,
  });
  return meetingId;
}

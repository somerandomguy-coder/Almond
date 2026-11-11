// In-memory `people.*` for the MockApi: a small catalog, speaker labels as `speaker_labels`
// artifacts (emitted through `artifact.added` like the real core), aggregation per person.
import type { Artifact } from "../../gen/Artifact";
import type { CalendarEvent } from "../../gen/CalendarEvent";
import type { Meeting } from "../../gen/Meeting";
import type { Person } from "../../gen/Person";
import type { SpeakerLabel } from "../../gen/SpeakerLabel";
import type { SpeakerLabels } from "../../gen/SpeakerLabels";
import type { Transcript } from "../../gen/Transcript";
import { ulid } from "../../lib/ids";
import { ApiError } from "../errors";
import type { LabelSpeakerParams, PersonDetail, PersonMeeting, PersonSummary, SavePersonParams } from "../types";
import { makeArtifact, thisDevice } from "./fixtures";

export interface PeopleHost {
  meetings(): Meeting[];
  artifacts(): Artifact[];
  addArtifact(a: Artifact): void;
  event(m: Meeting): CalendarEvent | null;
}

const KIND = "speaker_labels";
const MODEL = "sherpa:3dspeaker-eres2net-base";

function live(arts: Artifact[], meeting_id: string, kind: string): Artifact | undefined {
  const all = arts.filter((a) => a.meeting_id === meeting_id && a.kind === kind);
  const superseded = new Set(all.map((a) => a.supersedes).filter(Boolean));
  return all.filter((a) => !superseded.has(a.id)).sort((a, b) => b.created_at.wall_ms - a.created_at.wall_ms || b.created_at.logical - a.created_at.logical)[0];
}

const stripped = (p: Person): Person => ({ ...p, voiceprints: p.voiceprints.map((v) => ({ ...v, vector: [] })) });

export class MockPeople {
  people: Person[];

  constructor(
    private host: PeopleHost,
    now: number,
  ) {
    const print = (meeting_id: string, speaker: string) => ({ model: MODEL, vector: [], meeting_id, speaker, speech_s: 42, created_at: now });
    this.people = [
      { id: "self", name: "Nam Nguyễn", emails: ["nam.nguyen@gmail.com"], aliases: [], is_self: true, notes: "", voiceprints: [], created_at: now },
      { id: ulid(now - 3), name: "Trần Lan", emails: ["lan.tran@almond.dev"], aliases: ["Lan"], is_self: false, notes: "Marketing lead.", voiceprints: [], created_at: now },
      { id: ulid(now - 2), name: "David Kim", emails: ["david@almond.dev"], aliases: [], is_self: false, notes: "", voiceprints: [], created_at: now },
    ];
    // Q4 planning: me and Lan confirmed, David suggested by voice.
    const q4 = host.meetings().find((m) => m.title === "Họp kế hoạch Q4");
    const t = q4 && live(host.artifacts(), q4.id, "transcript");
    if (q4 && t) {
      const [me, lan, david] = this.people;
      lan.voiceprints.push(print(q4.id, "S1"));
      this.write(q4.id, t.id, [
        { speaker: "me", person: me.id, source: "user", confidence: null },
        { speaker: "S1", person: lan.id, source: "user", confidence: null },
        { speaker: "S2", person: david.id, source: "voice", confidence: 0.81 },
      ]);
    }
  }

  private person(id: string): Person {
    const p = this.people.find((x) => x.id === id);
    if (!p) throw new ApiError(-32004, `person ${id}`);
    return p;
  }

  private labels(meeting_id: string): SpeakerLabels | null {
    const a = live(this.host.artifacts(), meeting_id, KIND);
    return a ? (a.body as unknown as SpeakerLabels) : null;
  }

  private write(meeting_id: string, transcript: string, labels: SpeakerLabel[]): SpeakerLabels {
    const prev = live(this.host.artifacts(), meeting_id, KIND);
    const body: SpeakerLabels = { transcript, labels };
    this.host.addArtifact(makeArtifact({ meeting_id, device: thisDevice.id, wall: Date.now() }, KIND, body, { processor: "people.labels", inputs: [transcript] }, prev?.id ?? null));
    return body;
  }

  private meetingsOf(p: Person): PersonMeeting[] {
    const out: PersonMeeting[] = [];
    for (const m of this.host.meetings()) {
      const l = this.labels(m.id);
      const mine = (l?.labels ?? []).filter((x) => x.person === p.id);
      const ev = this.host.event(m);
      const attendee = !!ev?.attendees.some((a) => a.email && p.emails.includes(a.email.toLowerCase()));
      if (!mine.length && !attendee) continue;
      const t = l ? this.host.artifacts().find((a) => a.id === l.transcript) : undefined;
      const segs = t ? (t.body as unknown as Transcript).segments : [];
      const talk = segs.filter((s) => mine.some((x) => x.speaker === s.speaker)).reduce((n, s) => n + (s.t1_ms - s.t0_ms) / 1000, 0);
      const rank = { user: 3, voice: 2, calendar: 1 } as const;
      const source = mine.map((x) => x.source).sort((a, b) => rank[b] - rank[a])[0] ?? null;
      out.push({ meeting_id: m.id, title: m.title, started_at: m.started_at, speakers: mine.map((x) => x.speaker), talk_time_s: talk, source, attendee });
    }
    return out.sort((a, b) => b.started_at - a.started_at);
  }

  list(): PersonSummary[] {
    return this.people
      .map((p) => {
        const ms = this.meetingsOf(p);
        return {
          person: stripped(p),
          meeting_count: ms.length,
          talk_time_s: ms.reduce((n, m) => n + m.talk_time_s, 0),
          last_met: ms.length ? Math.max(...ms.map((m) => m.started_at)) : null,
        };
      })
      .sort((a, b) => Number(b.person.is_self) - Number(a.person.is_self) || (b.last_met ?? 0) - (a.last_met ?? 0) || a.person.name.localeCompare(b.person.name));
  }

  get(id: string): PersonDetail {
    const p = this.person(id);
    const meetings = this.meetingsOf(p);
    return {
      person: stripped(p),
      meetings,
      talk_time_s: meetings.reduce((n, m) => n + m.talk_time_s, 0),
      last_met: meetings.length ? Math.max(...meetings.map((m) => m.started_at)) : null,
    };
  }

  save(s: SavePersonParams): Person {
    const name = s.name.trim();
    if (!name) throw new ApiError(-32602, "a person needs a name");
    let p: Person;
    if (s.id) p = this.person(s.id);
    else {
      p = { id: ulid(), name, emails: [], aliases: [], is_self: false, notes: "", voiceprints: [], created_at: Date.now() };
      this.people.push(p);
    }
    p.name = name;
    if (s.emails) p.emails = [...new Set(s.emails.map((e) => e.trim().toLowerCase()).filter(Boolean))];
    if (s.aliases) p.aliases = s.aliases.map((a) => a.trim()).filter(Boolean);
    if (s.notes != null) p.notes = s.notes;
    if (s.voiceprints) {
      const keep = s.voiceprints;
      p.voiceprints = p.voiceprints.filter((v) => keep.some((k) => k.meeting_id === v.meeting_id && k.speaker === v.speaker && k.created_at === v.created_at));
    }
    if (s.is_self != null) {
      p.is_self = s.is_self;
      if (s.is_self) this.people.forEach((o) => o !== p && (o.is_self = false));
    }
    return stripped(p);
  }

  private replacePerson(from: string, to: string | null) {
    for (const m of this.host.meetings()) {
      const l = this.labels(m.id);
      if (!l?.labels.some((x) => x.person === from)) continue;
      const out: SpeakerLabel[] = [];
      for (const x of l.labels) {
        if (x.person === from && !to) continue;
        const y = x.person === from ? { ...x, person: to! } : x;
        if (!out.some((o) => o.speaker === y.speaker)) out.push(y);
      }
      this.write(m.id, l.transcript, out);
    }
  }

  delete(id: string): null {
    this.person(id);
    this.replacePerson(id, null);
    this.people = this.people.filter((p) => p.id !== id);
    return null;
  }

  merge(keep: string, remove: string): Person {
    if (keep === remove) throw new ApiError(-32602, "cannot merge a person with themself");
    const k = this.person(keep);
    const r = this.person(remove);
    k.aliases = [...new Set([...k.aliases, ...(r.name !== k.name ? [r.name] : []), ...r.aliases])];
    k.emails = [...new Set([...k.emails, ...r.emails])];
    k.notes = [k.notes, r.notes].filter((n) => n.trim()).join("\n\n");
    k.voiceprints = [...k.voiceprints, ...r.voiceprints];
    k.is_self ||= r.is_self;
    this.replacePerson(remove, keep);
    this.people = this.people.filter((p) => p.id !== remove);
    return stripped(k);
  }

  labelSpeaker(p: LabelSpeakerParams): SpeakerLabels {
    const name = p.name?.trim();
    if ([!!p.person_id, !!name, !!p.me].filter(Boolean).length !== 1) throw new ApiError(-32602, "give exactly one of person_id, name or me: true");
    const t = live(this.host.artifacts(), p.meeting_id, "transcript");
    if (!t) throw new ApiError(-32602, "the meeting has no transcript yet");
    let person: Person;
    if (p.me) person = this.people.find((x) => x.is_self) ?? this.person(this.save({ name: "Me", is_self: true }).id);
    else if (p.person_id) person = this.person(p.person_id);
    else person = this.people.find((x) => x.name.toLowerCase() === name!.toLowerCase()) ?? this.person(this.save({ name: name! }).id);
    const cur = this.labels(p.meeting_id);
    const labels = (cur?.transcript === t.id ? cur.labels : []).filter((x) => x.speaker !== p.speaker);
    labels.push({ speaker: p.speaker, person: person.id, source: "user", confidence: null });
    person.voiceprints = person.voiceprints.filter((v) => !(v.meeting_id === p.meeting_id && v.speaker === p.speaker));
    person.voiceprints.push({ model: MODEL, vector: [], meeting_id: p.meeting_id, speaker: p.speaker, speech_s: 30, created_at: Date.now() });
    return this.write(p.meeting_id, t.id, labels);
  }

  unlabelSpeaker(meeting_id: string, speaker: string): SpeakerLabels {
    const t = live(this.host.artifacts(), meeting_id, "transcript");
    if (!t) throw new ApiError(-32602, "the meeting has no transcript yet");
    const cur = this.labels(meeting_id);
    const old = cur?.labels.find((x) => x.speaker === speaker);
    if (old?.source === "user") {
      const p = this.people.find((x) => x.id === old.person);
      if (p) p.voiceprints = p.voiceprints.filter((v) => !(v.meeting_id === meeting_id && v.speaker === speaker));
    }
    return this.write(meeting_id, t.id, (cur?.transcript === t.id ? cur.labels : []).filter((x) => x.speaker !== speaker));
  }

  suggestions(): SpeakerLabel[] {
    return [];
  }
}

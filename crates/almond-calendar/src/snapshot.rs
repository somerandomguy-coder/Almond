//! Snapshot diffing for sources that only give us "everything in the window" (ICS feeds, the
//! phone): the cursor remembers `event_id → (start, fingerprint)` so each sync emits only what
//! changed, plus deletes for instances that disappeared *inside* the current window (events that
//! merely scrolled out of the window are forgotten, not deleted).

use almond_types::calendar::{CalendarDelta, CalendarEvent};
use almond_types::time::UnixMs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct SnapshotCursor {
    #[serde(default)]
    pub v: u32,
    /// HTTP ETag of the last fetch (ICS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// event_id → (start, fingerprint)
    #[serde(default)]
    pub instances: BTreeMap<String, (UnixMs, String)>,
}

impl SnapshotCursor {
    pub fn parse(cursor: Option<&str>) -> Self {
        cursor
            .and_then(|c| serde_json::from_str(c).ok())
            .unwrap_or_default()
    }

    pub fn to_json(&self) -> almond_types::Result<String> {
        Ok(serde_json::to_string(self)?)
    }
}

pub(crate) fn fingerprint(ev: &CalendarEvent) -> String {
    let bytes = serde_json::to_vec(ev).unwrap_or_default();
    blake3::hash(&bytes).to_hex()[..16].to_string()
}

/// Diff `current` (all instances in `[wstart, wend)`) against `prev`.
pub(crate) fn diff(
    prev: &SnapshotCursor,
    current: Vec<CalendarEvent>,
    wstart: UnixMs,
    wend: UnixMs,
) -> (CalendarDelta, SnapshotCursor) {
    let mut next = SnapshotCursor {
        v: 1,
        etag: None,
        instances: BTreeMap::new(),
    };
    let mut upserts = Vec::new();
    for ev in current {
        let fp = fingerprint(&ev);
        let changed = prev
            .instances
            .get(&ev.event_id)
            .is_none_or(|(_, old)| *old != fp);
        next.instances.insert(ev.event_id.clone(), (ev.start, fp));
        if changed {
            upserts.push(ev);
        }
    }
    let deletes = prev
        .instances
        .iter()
        .filter(|(id, (start, _))| {
            !next.instances.contains_key(*id) && *start >= wstart && *start < wend
        })
        .map(|(id, _)| id.clone())
        .collect();
    upserts.sort_by(|a, b| a.start.cmp(&b.start).then(a.event_id.cmp(&b.event_id)));
    (
        CalendarDelta {
            upserts,
            deletes,
            next_cursor: None,
            reset_calendars: vec![],
        },
        next,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, start: UnixMs, title: &str) -> CalendarEvent {
        CalendarEvent {
            source: "s".into(),
            event_id: id.into(),
            calendar_id: "c".into(),
            calendar_name: None,
            title: title.into(),
            description: None,
            location: None,
            start,
            end: start,
            all_day: false,
            attendees: vec![],
            conference_url: None,
            conference_app: None,
            organizer: None,
            status: "confirmed".into(),
            updated: 0,
        }
    }

    #[test]
    fn diffs() {
        let (d, c) = diff(
            &SnapshotCursor::default(),
            vec![ev("a", 10, "A"), ev("old", 1, "O"), ev("b", 20, "B")],
            0,
            100,
        );
        assert_eq!(d.upserts.len(), 3);
        // next sync: window moved to [5,100): "old" scrolled out, b removed, a renamed, c new
        let (d2, c2) = diff(&c, vec![ev("a", 10, "A2"), ev("c", 30, "C")], 5, 100);
        let ids: Vec<_> = d2.upserts.iter().map(|e| e.event_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(d2.deletes, vec!["b".to_string()]);
        assert_eq!(c2.instances.len(), 2);
        // unchanged → empty
        let (d3, _) = diff(&c2, vec![ev("a", 10, "A2"), ev("c", 30, "C")], 5, 100);
        assert!(d3.upserts.is_empty() && d3.deletes.is_empty());
    }
}

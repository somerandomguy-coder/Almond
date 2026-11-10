//! Assemble a `transcript` artifact body from per-channel segments + diarization.
//!
//! Channel prior (online meetings on desktop): the `mic` channel is the local user ("me"), the
//! `system` loopback channel is everyone else. So we only diarize the **system** channel, and the
//! mic channel is labelled `me` without running a model. When the user is on speakers instead of
//! headphones the mic also picks up the remote voices; those echo segments are detected by
//! time overlap + text similarity with system segments and dropped.
//!
//! Mixed audio (imports, phone recordings) is diarized as a whole.

use crate::metrics;
use almond_types::artifact::body::{Segment, Speaker, Transcript};
use almond_types::ids::SpeakerId;
use almond_types::meeting::AudioChannel;
use almond_types::stt::SpeakerTurn;

/// Speaker id used for the local user.
pub const ME: &str = "me";

fn overlap(a0: u32, a1: u32, b0: u32, b1: u32) -> u32 {
    a1.min(b1).saturating_sub(a0.max(b0))
}

/// Label each segment with the diarization speaker that overlaps it most (by time). Segments
/// with no overlapping turn get the nearest turn's speaker if it is within `max_gap_ms`.
pub fn assign_speakers(segments: &mut [Segment], turns: &[SpeakerTurn], max_gap_ms: u32) {
    for s in segments.iter_mut() {
        let mut best: Option<(&SpeakerId, u32)> = None;
        for t in turns {
            let o = overlap(s.t0_ms, s.t1_ms, t.t0_ms, t.t1_ms);
            if o > 0 && best.is_none_or(|(_, bo)| o > bo) {
                best = Some((&t.speaker, o));
            }
        }
        if best.is_none() {
            let mid = (s.t0_ms + s.t1_ms) / 2;
            best = turns
                .iter()
                .map(|t| {
                    let d = if mid < t.t0_ms {
                        t.t0_ms - mid
                    } else {
                        mid.saturating_sub(t.t1_ms)
                    };
                    (&t.speaker, d)
                })
                .filter(|(_, d)| *d <= max_gap_ms)
                .min_by_key(|(_, d)| *d)
                .map(|(sp, _)| (sp, 0));
        }
        if let Some((sp, _)) = best {
            s.speaker = Some(sp.clone());
        }
    }
}

/// Drop mic segments that are echoes of system audio (speakers instead of headphones):
/// time overlap >= 50% of the mic segment and character error rate between texts <= 0.5.
pub fn drop_echo(mic: Vec<Segment>, system: &[Segment]) -> Vec<Segment> {
    mic.into_iter()
        .filter(|m| {
            let dur = m.t1_ms.saturating_sub(m.t0_ms).max(1);
            let echo = system.iter().any(|s| {
                overlap(m.t0_ms, m.t1_ms, s.t0_ms, s.t1_ms) * 2 >= dur
                    && metrics::cer(&s.text, &m.text) <= 0.5
            });
            !echo
        })
        .collect()
}

/// Inputs for [`build_transcript`].
#[derive(Debug, Default)]
pub struct ChannelSegments {
    pub mic: Vec<Segment>,
    pub system: Vec<Segment>,
    pub mixed: Vec<Segment>,
    /// Diarization of the system channel (speaker ids are local, e.g. "S1").
    pub system_turns: Option<Vec<SpeakerTurn>>,
    /// Diarization of the mixed channel.
    pub mixed_turns: Option<Vec<SpeakerTurn>>,
}

/// Merge channels into one time-ordered transcript with speakers.
pub fn build_transcript(input: ChannelSegments) -> Transcript {
    let ChannelSegments {
        mic,
        mut system,
        mut mixed,
        system_turns,
        mixed_turns,
    } = input;

    let mut mic = drop_echo(mic, &system);
    for s in mic.iter_mut() {
        s.speaker = Some(SpeakerId(ME.into()));
    }

    // Remote speakers: diarized ids prefixed so they can't clash with mixed-channel ids.
    match &system_turns {
        Some(turns) if !turns.is_empty() => {
            let turns: Vec<SpeakerTurn> = turns
                .iter()
                .map(|t| SpeakerTurn {
                    speaker: SpeakerId(format!("remote:{}", t.speaker.0)),
                    ..t.clone()
                })
                .collect();
            assign_speakers(&mut system, &turns, 2000);
        }
        _ => {
            for s in system.iter_mut() {
                s.speaker = Some(SpeakerId("remote".into()));
            }
        }
    }
    if let Some(turns) = &mixed_turns {
        assign_speakers(&mut mixed, turns, 2000);
    }

    let mut segments: Vec<Segment> = mic.into_iter().chain(system).chain(mixed).collect();
    segments.sort_by(|a, b| {
        (a.t0_ms, a.t1_ms, channel_rank(a.channel)).cmp(&(
            b.t0_ms,
            b.t1_ms,
            channel_rank(b.channel),
        ))
    });
    for (i, s) in segments.iter_mut().enumerate() {
        s.idx = i as u32;
    }

    // Speakers in order of first appearance.
    let mut speakers: Vec<Speaker> = vec![];
    for s in &segments {
        if let Some(id) = &s.speaker
            && !speakers.iter().any(|x| &x.id == id)
        {
            speakers.push(Speaker {
                id: id.clone(),
                name: None,
                email: None,
                is_me: id.0 == ME,
            });
        }
    }
    Transcript { segments, speakers }
}

fn channel_rank(c: AudioChannel) -> u8 {
    match c {
        AudioChannel::Mic => 0,
        AudioChannel::System => 1,
        AudioChannel::Mixed => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(t0: u32, t1: u32, ch: AudioChannel, text: &str) -> Segment {
        Segment {
            idx: 0,
            t0_ms: t0,
            t1_ms: t1,
            channel: ch,
            speaker: None,
            lang: None,
            text: text.into(),
            confidence: None,
            words: None,
        }
    }

    fn turn(t0: u32, t1: u32, s: &str) -> SpeakerTurn {
        SpeakerTurn {
            t0_ms: t0,
            t1_ms: t1,
            speaker: SpeakerId(s.into()),
        }
    }

    #[test]
    fn mic_is_me_system_is_diarized_and_sorted() {
        let t = build_transcript(ChannelSegments {
            mic: vec![seg(5000, 7000, AudioChannel::Mic, "tôi đồng ý")],
            system: vec![
                seg(0, 4000, AudioChannel::System, "chào mọi người"),
                seg(8000, 9000, AudioChannel::System, "ok cảm ơn"),
            ],
            system_turns: Some(vec![turn(0, 4100, "S0"), turn(7900, 9100, "S1")]),
            ..Default::default()
        });
        let who: Vec<_> = t
            .segments
            .iter()
            .map(|s| s.speaker.as_ref().unwrap().0.as_str())
            .collect();
        assert_eq!(who, vec!["remote:S0", "me", "remote:S1"]);
        assert_eq!(
            t.segments.iter().map(|s| s.idx).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(t.speakers.len(), 3);
        assert!(t.speakers.iter().any(|s| s.is_me && s.id.0 == "me"));
    }

    #[test]
    fn echo_on_mic_is_dropped() {
        let t = build_transcript(ChannelSegments {
            mic: vec![
                seg(0, 3000, AudioChannel::Mic, "chào mọi người nhé"),
                seg(4000, 5000, AudioChannel::Mic, "vâng"),
            ],
            system: vec![seg(100, 3100, AudioChannel::System, "Chào mọi người nhé.")],
            ..Default::default()
        });
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[1].text, "vâng");
        assert_eq!(t.segments[0].speaker.as_ref().unwrap().0, "remote");
    }

    #[test]
    fn nearest_turn_within_gap() {
        let mut s = vec![seg(5000, 5500, AudioChannel::Mixed, "x")];
        assign_speakers(&mut s, &[turn(0, 4000, "A"), turn(6200, 7000, "B")], 2000);
        assert_eq!(s[0].speaker.as_ref().unwrap().0, "B");
        let mut s = vec![seg(50_000, 51_000, AudioChannel::Mixed, "x")];
        assign_speakers(&mut s, &[turn(0, 4000, "A")], 2000);
        assert!(s[0].speaker.is_none());
    }
}

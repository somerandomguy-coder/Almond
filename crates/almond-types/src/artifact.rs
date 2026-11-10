//! Artifacts: immutable, provenance-carrying outputs of processors (transcripts, summaries,
//! claims, verdicts, ...). See docs/ARCHITECTURE.md §4–5.
//!
//! `Artifact::body` is JSON so that unknown kinds/schemas from newer devices survive sync. Use the
//! typed body structs in [`body`] plus [`Artifact::decode`] / [`ArtifactDraft::new`] for known kinds.

use crate::ids::{DeviceId, Id, SpeakerId};
use crate::time::Hlc;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Open set of artifact kinds. Well-known kinds are associated consts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArtifactKind(pub String);

impl ArtifactKind {
    pub const TRANSCRIPT: &'static str = "transcript";
    pub const SUMMARY: &'static str = "summary";
    pub const ENHANCED_NOTES: &'static str = "enhanced_notes";
    pub const ACTION_ITEMS: &'static str = "action_items";
    pub const TITLE: &'static str = "title";
    pub const CLAIMS: &'static str = "claims";
    pub const VERDICTS: &'static str = "verdicts";
    pub const ACTION_PROPOSALS: &'static str = "action_proposals";
    pub const ACTION_RESULT: &'static str = "action_result";

    pub fn new(s: &str) -> Self {
        ArtifactKind(s.to_string())
    }
    pub fn is(&self, s: &str) -> bool {
        self.0 == s
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Artifact {
    pub id: Id,
    pub meeting_id: Id,
    pub kind: ArtifactKind,
    /// Body schema version for this kind. Bump when the body shape changes incompatibly.
    pub schema: u32,
    pub body: serde_json::Value,
    pub provenance: Provenance,
    /// Previous artifact this one replaces (rerun / correction). Old one stays for comparison.
    pub supersedes: Option<Id>,
    pub created_at: Hlc,
    pub device: DeviceId,
}

impl Artifact {
    /// Decode the body into a typed struct. Fails if the kind doesn't match `T::KIND`.
    pub fn decode<T: ArtifactBody>(&self) -> crate::Result<T> {
        if !self.kind.is(T::KIND) {
            return Err(crate::Error::Invalid(format!(
                "artifact {} is {:?}, expected {}",
                self.id,
                self.kind.0,
                T::KIND
            )));
        }
        Ok(serde_json::from_value(self.body.clone())?)
    }
}

/// What a processor returns; the pipeline assigns id / created_at / device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ArtifactDraft {
    pub meeting_id: Id,
    pub kind: ArtifactKind,
    pub schema: u32,
    pub body: serde_json::Value,
    pub provenance: Provenance,
    pub supersedes: Option<Id>,
}

impl ArtifactDraft {
    pub fn new<T: ArtifactBody>(
        meeting_id: Id,
        body: &T,
        provenance: Provenance,
    ) -> crate::Result<Self> {
        Ok(ArtifactDraft {
            meeting_id,
            kind: ArtifactKind::new(T::KIND),
            schema: T::SCHEMA,
            body: serde_json::to_value(body)?,
            provenance,
            supersedes: None,
        })
    }

    pub fn into_artifact(self, id: Id, created_at: Hlc, device: DeviceId) -> Artifact {
        Artifact {
            id,
            meeting_id: self.meeting_id,
            kind: self.kind,
            schema: self.schema,
            body: self.body,
            provenance: self.provenance,
            supersedes: self.supersedes,
            created_at,
            device,
        }
    }
}

/// Implemented by every typed artifact body.
pub trait ArtifactBody: Serialize + DeserializeOwned {
    const KIND: &'static str;
    const SCHEMA: u32;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct Provenance {
    /// Processor name, e.g. "transcribe.whisper", "summarize.llm", "verify.grounding".
    pub processor: String,
    pub processor_version: String,
    /// e.g. "ollama:qwen3.5:9b@<digest>", "whisper:phowhisper-large@<sha256>".
    pub model: Option<String>,
    /// BLAKE3 of (prompt template id + params) so reruns are comparable.
    pub params_hash: Option<String>,
    /// Artifacts consumed (transcripts, summaries, ...). Audio is referenced via the meeting.
    pub inputs: Vec<Id>,
}

/// `Provenance::params_hash`: BLAKE3 hex of the prompt/template id and the job params, so reruns
/// with the same settings are recognisably identical across processors and devices.
pub fn params_hash(template_id: &str, params: &serde_json::Value) -> String {
    let mut h = blake3::Hasher::new();
    h.update(template_id.as_bytes());
    h.update(&[0]);
    h.update(params.to_string().as_bytes());
    h.finalize().to_hex().to_string()
}

/// The grounding primitive: points into a segment of another artifact (usually a transcript).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SpanRef {
    pub artifact: Id,
    /// Segment / bullet / block index inside that artifact.
    pub segment: u32,
    /// Optional char range inside the segment text (UTF-8 char indices, end exclusive).
    pub chars: Option<(u32, u32)>,
    /// Optional audio time range in ms relative to meeting start.
    pub time_ms: Option<(u32, u32)>,
}

pub mod body {
    //! Typed bodies for the well-known artifact kinds.
    use super::*;
    use crate::lang::LangTag;
    use crate::meeting::AudioChannel;

    // ---------------- transcript ----------------
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
    #[ts(export)]
    pub struct Transcript {
        pub segments: Vec<Segment>,
        /// Speaker id -> display name, filled by diarization / user edits.
        pub speakers: Vec<Speaker>,
    }
    impl ArtifactBody for Transcript {
        const KIND: &'static str = ArtifactKind::TRANSCRIPT;
        const SCHEMA: u32 = 1;
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct Segment {
        pub idx: u32,
        pub t0_ms: u32,
        pub t1_ms: u32,
        pub channel: AudioChannel,
        pub speaker: Option<SpeakerId>,
        pub lang: Option<LangTag>,
        pub text: String,
        /// Mean token probability or engine confidence in [0,1].
        pub confidence: Option<f32>,
        pub words: Option<Vec<Word>>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct Word {
        pub t0_ms: u32,
        pub t1_ms: u32,
        pub text: String,
        pub p: Option<f32>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct Speaker {
        pub id: SpeakerId,
        pub name: Option<String>,
        pub email: Option<String>,
        pub is_me: bool,
    }

    // ---------------- summary ----------------
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
    #[ts(export)]
    pub struct Summary {
        pub lang: Option<LangTag>,
        pub sections: Vec<SummarySection>,
    }
    impl ArtifactBody for Summary {
        const KIND: &'static str = ArtifactKind::SUMMARY;
        const SCHEMA: u32 = 1;
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct SummarySection {
        pub heading: String,
        pub bullets: Vec<CitedText>,
    }

    /// A piece of AI-generated text with the transcript spans that support it.
    /// **Every** AI bullet must carry `cites` (may be empty only if truly unsupported).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct CitedText {
        pub text: String,
        pub cites: Vec<SpanRef>,
    }

    // ---------------- enhanced notes ----------------
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
    #[ts(export)]
    pub struct EnhancedNotes {
        pub lang: Option<LangTag>,
        pub blocks: Vec<NoteBlock>,
    }
    impl ArtifactBody for EnhancedNotes {
        const KIND: &'static str = ArtifactKind::ENHANCED_NOTES;
        const SCHEMA: u32 = 1;
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct NoteBlock {
        pub origin: NoteOrigin,
        /// "heading" | "paragraph" | "bullet" | "todo"
        pub style: String,
        /// Nesting level for bullets (0 = top).
        pub level: u8,
        pub text: String,
        pub cites: Vec<SpanRef>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[serde(rename_all = "snake_case")]
    #[ts(export)]
    pub enum NoteOrigin {
        /// Typed by the user — must be preserved verbatim.
        User,
        /// Added by the AI.
        Ai,
    }

    // ---------------- action items ----------------
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
    #[ts(export)]
    pub struct ActionItems {
        pub items: Vec<ActionItem>,
    }
    impl ArtifactBody for ActionItems {
        const KIND: &'static str = ArtifactKind::ACTION_ITEMS;
        const SCHEMA: u32 = 1;
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct ActionItem {
        pub text: String,
        pub owner: Option<String>,
        /// ISO-8601 date or free text ("next Friday") if unresolved.
        pub due: Option<String>,
        pub cites: Vec<SpanRef>,
    }

    // ---------------- title ----------------
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct Title {
        pub title: String,
    }
    impl ArtifactBody for Title {
        const KIND: &'static str = ArtifactKind::TITLE;
        const SCHEMA: u32 = 1;
    }

    // ---------------- claims / verdicts (verification layer) ----------------
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
    #[ts(export)]
    pub struct Claims {
        pub claims: Vec<Claim>,
    }
    impl ArtifactBody for Claims {
        const KIND: &'static str = ArtifactKind::CLAIMS;
        const SCHEMA: u32 = 1;
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct Claim {
        /// Stable within the claims artifact ("c1", "c2", ...).
        pub id: String,
        /// Atomic, checkable statement in natural language.
        pub text: String,
        /// Where the claim came from. For summaries: `artifact` = the summary's id and `segment` =
        /// the bullet's flat index across all sections (section 0's bullets first, then 1, ...).
        pub source: SpanRef,
        /// Evidence the generator cited.
        pub evidence: Vec<SpanRef>,
        /// Optional structured form for symbolic reasoning (triples, Datalog facts, ...).
        /// Shape is defined by `logical_form_kind`; free for future neuro-symbolic work.
        pub logical_form_kind: Option<String>,
        pub logical_form: Option<serde_json::Value>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, Default)]
    #[ts(export)]
    pub struct Verdicts {
        pub verdicts: Vec<Verdict>,
        /// Verifiers that failed; the others' verdicts still stand.
        #[serde(default)]
        pub errors: Vec<VerifierError>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct VerifierError {
        pub verifier: String,
        pub message: String,
    }
    impl ArtifactBody for Verdicts {
        const KIND: &'static str = ArtifactKind::VERDICTS;
        const SCHEMA: u32 = 1;
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
    #[ts(export)]
    pub struct Verdict {
        /// (claims artifact id, claim id)
        pub claims_artifact: Id,
        pub claim_id: String,
        /// Verifier name, e.g. "verify.citation_exists", "verify.numbers", "verify.nli".
        pub verifier: String,
        pub label: VerdictLabel,
        /// [0,1]
        pub confidence: f32,
        pub evidence: Vec<SpanRef>,
        pub explanation: String,
        /// Optional machine-readable derivation / proof (symbolic verifiers).
        pub trace: Option<serde_json::Value>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
    #[serde(rename_all = "snake_case")]
    #[ts(export)]
    pub enum VerdictLabel {
        Supported,
        Contradicted,
        Unverifiable,
        NeedsReview,
    }
}

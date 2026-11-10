//! Post-meeting actions (ARCHITECTURE §8b). Built-in: `export.markdown`, which writes the
//! meeting's title, summary, action items, enhanced notes and fact-check flags to a Markdown file.
//!
//! Proposals are deterministic per (action, meeting) so `actions.propose` only records a new
//! `action_proposals` artifact when the content changed. Executing records an `action_result`.

use crate::CoreConfig;
use almond_llm::tasks::iso_date;
use almond_pipeline::planner::latest_live;
use almond_types::actions::{Action, ActionDescriptor, ActionProposal, ActionResult};
use almond_types::artifact::body::{
    ActionItems, EnhancedNotes, NoteOrigin, Summary, Title, VerdictLabel, Verdicts,
};
use almond_types::{Artifact, ArtifactKind, Error, Meeting, Result};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

pub const EXPORT_MARKDOWN: &str = "export.markdown";

pub(crate) struct Actions {
    pub list: Vec<Arc<dyn Action>>,
}

impl Actions {
    pub(crate) fn new(cfg: &CoreConfig) -> Self {
        Actions {
            list: vec![Arc::new(ExportMarkdown {
                dir: export_dir(cfg),
            })],
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<Arc<dyn Action>> {
        self.list.iter().find(|a| a.descriptor().id == id).cloned()
    }
}

/// `~/Documents/Almond` on desktops, else `<data_dir>/exports`.
fn export_dir(cfg: &CoreConfig) -> PathBuf {
    if cfg.platform != "android" {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
        if let Some(h) = home {
            let docs = PathBuf::from(h).join("Documents");
            if docs.is_dir() {
                return docs.join("Almond");
            }
        }
    }
    cfg.data_dir.join("exports")
}

pub struct ExportMarkdown {
    pub dir: PathBuf,
}

fn file_name(m: &Meeting, title: &str) -> String {
    let safe: String = title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let safe: String = safe.chars().take(80).collect();
    let safe = if safe.is_empty() {
        "meeting".to_string()
    } else {
        safe
    };
    format!("{} {safe}.md", iso_date(m.started_at))
}

/// Meeting title: the meeting's own, else the latest `title` artifact.
fn display_title(m: &Meeting, arts: &[Artifact]) -> String {
    if !m.title.trim().is_empty() {
        return m.title.trim().to_string();
    }
    latest_live(arts, ArtifactKind::TITLE)
        .and_then(|a| a.decode::<Title>().ok())
        .map(|t| t.title)
        .unwrap_or_else(|| "Meeting".into())
}

/// Markdown for a meeting from its live artifacts.
pub fn render_markdown(m: &Meeting, arts: &[Artifact]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}\n", display_title(m, arts));
    let _ = writeln!(out, "_{}_\n", iso_date(m.started_at));

    let summary = latest_live(arts, ArtifactKind::SUMMARY);
    // Flags per summary bullet (flat index) from the aggregated verdicts of the claims
    // extracted from this summary.
    let mut flags: HashMap<u32, VerdictLabel> = HashMap::new();
    if let (Some(s), Some(c), Some(v)) = (
        summary,
        latest_live(arts, ArtifactKind::CLAIMS),
        latest_live(arts, ArtifactKind::VERDICTS),
    ) && let (Ok(claims), Ok(verdicts)) = (
        c.decode::<almond_types::artifact::body::Claims>(),
        v.decode::<Verdicts>(),
    ) {
        for vd in verdicts
            .verdicts
            .iter()
            .filter(|x| x.verifier == "verify.aggregate" && x.claims_artifact == c.id)
        {
            if let Some(cl) = claims
                .claims
                .iter()
                .find(|cl| cl.id == vd.claim_id && cl.source.artifact == s.id)
                && vd.label != VerdictLabel::Supported
            {
                flags.insert(cl.source.segment, vd.label);
            }
        }
    }
    if let Some(s) = summary.and_then(|a| a.decode::<Summary>().ok()) {
        let _ = writeln!(out, "## Summary\n");
        let mut i = 0u32;
        for sec in &s.sections {
            let _ = writeln!(out, "### {}\n", sec.heading);
            for b in &sec.bullets {
                let flag = match flags.get(&i) {
                    Some(VerdictLabel::Contradicted) => " ⚠ (contradicted by the transcript)",
                    Some(VerdictLabel::NeedsReview) => " ⚠ (needs review)",
                    Some(VerdictLabel::Unverifiable) => " (unverified)",
                    _ => "",
                };
                let _ = writeln!(out, "- {}{flag}", b.text);
                i += 1;
            }
            out.push('\n');
        }
    }
    if let Some(a) = latest_live(arts, ArtifactKind::ACTION_ITEMS)
        .and_then(|a| a.decode::<ActionItems>().ok())
        .filter(|a| !a.items.is_empty())
    {
        let _ = writeln!(out, "## Action items\n");
        for it in &a.items {
            let mut extra = vec![];
            if let Some(o) = it.owner.as_deref().filter(|o| !o.is_empty()) {
                extra.push(format!("@{o}"));
            }
            if let Some(d) = it.due.as_deref().filter(|d| !d.is_empty()) {
                extra.push(format!("due {d}"));
            }
            let extra = if extra.is_empty() {
                String::new()
            } else {
                format!(" ({})", extra.join(", "))
            };
            let _ = writeln!(out, "- [ ] {}{extra}", it.text);
        }
        out.push('\n');
    }
    if let Some(n) = latest_live(arts, ArtifactKind::ENHANCED_NOTES)
        .and_then(|a| a.decode::<EnhancedNotes>().ok())
        .filter(|n| !n.blocks.is_empty())
    {
        let _ = writeln!(out, "## Notes\n");
        for b in &n.blocks {
            let ai = if b.origin == NoteOrigin::Ai {
                " *(AI)*"
            } else {
                ""
            };
            let indent = "  ".repeat(b.level as usize);
            match b.style.as_str() {
                "heading" => {
                    let _ = writeln!(out, "\n### {}{ai}\n", b.text);
                }
                "todo" => {
                    let _ = writeln!(out, "{indent}- [ ] {}{ai}", b.text);
                }
                "paragraph" => {
                    let _ = writeln!(out, "{}{ai}\n", b.text);
                }
                _ => {
                    let _ = writeln!(out, "{indent}- {}{ai}", b.text);
                }
            }
        }
        out.push('\n');
    }
    out
}

#[async_trait]
impl Action for ExportMarkdown {
    fn descriptor(&self) -> ActionDescriptor {
        ActionDescriptor {
            id: EXPORT_MARKDOWN.into(),
            title: "Export Markdown".into(),
            requires: vec![],
        }
    }

    async fn propose(&self, m: &Meeting, arts: &[Artifact]) -> Result<Vec<ActionProposal>> {
        if latest_live(arts, ArtifactKind::SUMMARY).is_none()
            && latest_live(arts, ArtifactKind::ENHANCED_NOTES).is_none()
        {
            return Ok(vec![]);
        }
        let md = render_markdown(m, arts);
        let name = file_name(m, &display_title(m, arts));
        let path = self.dir.join(&name);
        Ok(vec![ActionProposal {
            id: format!("{EXPORT_MARKDOWN}:{}", m.id),
            action: EXPORT_MARKDOWN.into(),
            title: format!("Export Markdown → {}", path.display()),
            preview: md.clone(),
            payload: json!({ "meeting_id": m.id, "path": path, "markdown": md }),
        }])
    }

    async fn execute(&self, p: &ActionProposal) -> Result<ActionResult> {
        let md = p.payload["markdown"]
            .as_str()
            .ok_or_else(|| Error::Invalid("export.markdown: payload.markdown missing".into()))?;
        let path = match p.payload["path"].as_str() {
            Some(s) if !s.trim().is_empty() => PathBuf::from(s),
            _ => self.dir.join("meeting.md"),
        };
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            return Err(Error::Invalid(format!(
                "export.markdown writes .md files only, got {}",
                path.display()
            )));
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, md)?;
        Ok(ActionResult {
            proposal_id: p.id.clone(),
            ok: true,
            message: format!("Saved to {}", path.display()),
            url: Some(format!(
                "file:///{}",
                path.display()
                    .to_string()
                    .replace('\\', "/")
                    .trim_start_matches('/')
            )),
        })
    }
}

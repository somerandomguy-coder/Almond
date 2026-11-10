//! Summary eval harness (`eval/summary`). Needs a running Ollama.
//!
//! ```text
//! cargo run --release -p almond-llm --example eval_summary -- \
//!     --models qwen3.5:9b,gemma4:e4b --judge qwen3.5:9b [--template-lang en] [--cross]
//! ```
//! Metrics per (model, transcript):
//! - citation validity: valid cites / raw cites returned by the model
//! - grounded: bullets with >= 1 valid cite
//! - cite hit: for each key point, the best-matching bullet cites one of its gold segments (±1)
//! - coverage (keyword): key points whose keyword groups all appear in the summary
//! - coverage (judge): key points an LLM judge says the summary states (optional `--judge`)
//! - language: bullets detected as the requested output language
//! - action-item recall (keyword) and latency / tokens per second
//!
//! `--cross` also summarizes every transcript into the *other* language (vi->en, en->vi) and
//! reports language correctness + citation validity for that.

use almond_llm::{Hooks, LlmTasks, Ollama, PromptLibrary, TaskConfig, TaskInput, TranscriptView};
use almond_types::artifact::body::{Segment, Summary, Transcript};
use almond_types::ids::SpeakerId;
use almond_types::llm::{ChatMessage, ChatRequest, LlmProvider, Role};
use almond_types::meeting::AudioChannel;
use almond_types::{ArtifactDraft, DeviceId, Id, LangTag, Provenance};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Deserialize)]
struct Case {
    id: String,
    lang: String,
    title: String,
    date: String,
    segments: Vec<(String, String)>,
    key_points: Vec<KeyPoint>,
    action_items: Vec<GoldAction>,
}

#[derive(Deserialize)]
struct KeyPoint {
    point: String,
    keywords: Vec<Vec<String>>,
    segments: Vec<u32>,
}

#[derive(Deserialize)]
struct GoldAction {
    keywords: Vec<Vec<String>>,
}

#[derive(Serialize, Default, Clone)]
struct Row {
    model: String,
    case: String,
    lang: String,
    out_lang: String,
    ok: bool,
    error: Option<String>,
    bullets: u32,
    cites_total: u32,
    cites_valid: u32,
    grounded: f64,
    cite_hit: f64,
    coverage_kw: f64,
    coverage_judge: Option<f64>,
    lang_ok: f64,
    ai_recall: Option<f64>,
    summary_ms: u64,
    actions_ms: u64,
    completion_tokens: u32,
    tok_per_s: f64,
    summary: Option<Summary>,
}

fn fold(s: &str) -> String {
    const MAP: [(&str, char); 7] = [
        ("àáảãạăằắẳẵặâầấẩẫậ", 'a'),
        ("èéẻẽẹêềếểễệ", 'e'),
        ("ìíỉĩị", 'i'),
        ("òóỏõọôồốổỗộơờớởỡợ", 'o'),
        ("ùúủũụưừứửữự", 'u'),
        ("ỳýỷỹỵ", 'y'),
        ("đ", 'd'),
    ];
    s.to_lowercase()
        .chars()
        .map(|c| {
            MAP.iter()
                .find(|(set, _)| set.contains(c))
                .map(|(_, b)| *b)
                .unwrap_or(c)
        })
        .collect()
}

fn matches_group(text_folded: &str, group: &[String]) -> bool {
    group.iter().any(|k| text_folded.contains(&fold(k)))
}

fn groups_matched(text: &str, groups: &[Vec<String>]) -> usize {
    let f = fold(text);
    groups.iter().filter(|g| matches_group(&f, g)).count()
}

const VI_CHARS: &str = "àáảãạăằắẳẵặâầấẩẫậèéẻẽẹêềếểễệìíỉĩịòóỏõọôồốổỗộơờớởỡợùúủũụưừứửữựỳýỷỹỵđ";

/// "vi" if >= 30% of words carry Vietnamese diacritics, else "en".
fn detect_lang(text: &str) -> &'static str {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return "en";
    }
    let vi = words
        .iter()
        .filter(|w| w.to_lowercase().chars().any(|c| VI_CHARS.contains(c)))
        .count();
    if vi as f64 / words.len() as f64 >= 0.3 {
        "vi"
    } else {
        "en"
    }
}

fn build_view(c: &Case) -> TranscriptView {
    let mut t0 = 0u32;
    let segments = c
        .segments
        .iter()
        .enumerate()
        .map(|(i, (spk, text))| {
            let dur = 1500 + 60 * text.chars().count() as u32;
            let s = Segment {
                idx: i as u32,
                t0_ms: t0,
                t1_ms: t0 + dur,
                channel: AudioChannel::Mixed,
                speaker: Some(SpeakerId(spk.clone())),
                lang: Some(LangTag(c.lang.clone())),
                text: text.clone(),
                confidence: None,
                words: None,
            };
            t0 += dur + 300;
            s
        })
        .collect();
    let body = Transcript {
        segments,
        speakers: vec![],
    };
    let a = ArtifactDraft::new(Id::new(), &body, Provenance::default())
        .unwrap()
        .into_artifact(Id::new(), Default::default(), DeviceId("eval".into()));
    TranscriptView::from_artifact(&a).unwrap()
}

async fn judge(
    llm: &Arc<dyn LlmProvider>,
    model: &str,
    summary_text: &str,
    kps: &[KeyPoint],
) -> Option<f64> {
    let list: String = kps
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{}. {}\n", i + 1, k.point))
        .collect();
    let prompt = format!(
        "You grade meeting summaries. The summary may be in Vietnamese or English.\n\nSummary:\n{summary_text}\n\nKey points:\n{list}\nFor each key point, answer true if the summary clearly states it (same facts; numbers and names must match), otherwise false. Return JSON {{\"covered\": [{} booleans in order]}}.",
        kps.len()
    );
    let req = ChatRequest {
        model: model.into(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: prompt,
        }],
        json_schema: Some(json!({
            "type": "object",
            "properties": {"covered": {"type": "array", "items": {"type": "boolean"}}},
            "required": ["covered"]
        })),
        temperature: Some(0.0),
        num_ctx: None,
        think: Some(false),
    };
    let r = llm.chat(req).await.ok()?;
    let v = almond_llm::schema::extract_json(&r.content)?;
    let arr = v["covered"].as_array()?;
    let n = arr.len().min(kps.len());
    if n == 0 {
        return None;
    }
    Some(
        arr[..n]
            .iter()
            .filter(|b| b.as_bool() == Some(true))
            .count() as f64
            / kps.len() as f64,
    )
}

fn summary_text(s: &Summary) -> String {
    let mut t = String::new();
    for sec in &s.sections {
        t.push_str(&format!("## {}\n", sec.heading));
        for b in &sec.bullets {
            t.push_str(&format!("- {}\n", b.text));
        }
    }
    t
}

#[allow(clippy::too_many_arguments)]
async fn run_case(
    llm: &Arc<dyn LlmProvider>,
    model: &str,
    template_lang: Option<&str>,
    judge_model: Option<&str>,
    c: &Case,
    out_lang: &str,
    with_actions: bool,
) -> Row {
    let view = build_view(c);
    let tasks = LlmTasks::new(
        llm.clone(),
        PromptLibrary::builtin(),
        TaskConfig {
            model: model.into(),
            template_lang: template_lang.map(|l| LangTag(l.into())),
            ..Default::default()
        },
    );
    let input = TaskInput {
        transcript: &view,
        meeting_title: c.title.clone(),
        meeting_date: Some(c.date.clone()),
        output_lang: LangTag(out_lang.into()),
    };
    let mut row = Row {
        model: model.into(),
        case: c.id.clone(),
        lang: c.lang.clone(),
        out_lang: out_lang.into(),
        ..Default::default()
    };
    let s = match tasks.summarize(&input, &Hooks::default()).await {
        Ok(s) => s,
        Err(e) => {
            row.error = Some(e.to_string());
            return row;
        }
    };
    row.ok = true;
    let bullets: Vec<_> = s.body.sections.iter().flat_map(|x| &x.bullets).collect();
    row.bullets = bullets.len() as u32;
    row.cites_total = s.report.cites.cites_total;
    row.cites_valid = s.report.cites.cites_valid;
    row.grounded = s.report.cites.grounded();
    row.summary_ms = s.report.elapsed_ms;
    row.completion_tokens = s.report.completion_tokens;
    row.tok_per_s =
        s.report.completion_tokens as f64 / (s.report.elapsed_ms.max(1) as f64 / 1000.0);
    row.lang_ok = if bullets.is_empty() {
        0.0
    } else {
        bullets
            .iter()
            .filter(|b| detect_lang(&b.text) == out_lang)
            .count() as f64
            / bullets.len() as f64
    };
    let text = summary_text(&s.body);
    if out_lang == c.lang {
        let covered = c
            .key_points
            .iter()
            .filter(|k| groups_matched(&text, &k.keywords) == k.keywords.len())
            .count();
        row.coverage_kw = covered as f64 / c.key_points.len() as f64;
        let (mut with_best, mut hits) = (0, 0);
        for k in &c.key_points {
            let best = bullets
                .iter()
                .map(|b| (groups_matched(&b.text, &k.keywords), b))
                .filter(|(n, _)| *n > 0)
                .max_by_key(|(n, _)| *n);
            if let Some((_, b)) = best {
                with_best += 1;
                if b.cites.iter().any(|c| {
                    k.segments
                        .iter()
                        .any(|g| (c.segment as i64 - *g as i64).abs() <= 1)
                }) {
                    hits += 1;
                }
            }
        }
        row.cite_hit = if with_best == 0 {
            0.0
        } else {
            hits as f64 / with_best as f64
        };
        if let Some(j) = judge_model {
            row.coverage_judge = judge(llm, j, &text, &c.key_points).await;
            if row.coverage_judge.is_none() {
                row.error = Some(format!("judge {j} failed"));
            }
        }
    }
    if with_actions {
        match tasks.action_items(&input, &Hooks::default()).await {
            Ok(a) => {
                row.actions_ms = a.report.elapsed_ms;
                let all: String = a
                    .body
                    .items
                    .iter()
                    .map(|i| format!("{} {}\n", i.text, i.owner.clone().unwrap_or_default()))
                    .collect();
                let found = c
                    .action_items
                    .iter()
                    .filter(|g| groups_matched(&all, &g.keywords) == g.keywords.len())
                    .count();
                row.ai_recall = Some(found as f64 / c.action_items.len().max(1) as f64);
            }
            Err(e) => row.error = Some(format!("action_items: {e}")),
        }
    }
    row.summary = Some(s.body);
    row
}

fn mean(xs: impl Iterator<Item = f64>) -> f64 {
    let v: Vec<f64> = xs.collect();
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let models: Vec<String> = arg("--models")
        .unwrap_or_else(|| "qwen3.5:9b,gemma4:e4b".into())
        .split(',')
        .map(str::to_string)
        .collect();
    let judge_model = arg("--judge");
    let template_lang = arg("--template-lang");
    let cross = args.iter().any(|a| a == "--cross");
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../eval/summary");
    let data = arg("--data")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("data"));
    let out = arg("--out")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("results"));
    std::fs::create_dir_all(&out).unwrap();

    let mut files: Vec<_> = std::fs::read_dir(&data)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let cases: Vec<Case> = files
        .iter()
        .map(|p| serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap())
        .collect();
    let llm: Arc<dyn LlmProvider> = Arc::new(Ollama::from_env().unwrap());
    // Fail loudly up front if the server or any model (incl. the judge) is missing.
    let have: Vec<String> = match llm.list_models().await {
        Ok(v) => v.into_iter().map(|m| m.name).collect(),
        Err(e) => {
            eprintln!("ERROR: cannot reach Ollama: {e}");
            std::process::exit(2);
        }
    };
    let missing: Vec<&String> = models
        .iter()
        .chain(judge_model.iter())
        .filter(|m| !have.iter().any(|h| h == *m))
        .collect();
    if !missing.is_empty() {
        eprintln!("ERROR: models not pulled on this Ollama: {missing:?} (have: {have:?})");
        std::process::exit(2);
    }

    let mut rows: Vec<Row> = vec![];
    for m in &models {
        // Warm-up so model load time doesn't count as latency.
        let w = run_case(&llm, m, None, None, &cases[0], &cases[0].lang, false).await;
        if let Some(e) = w.error {
            eprintln!("ERROR: warm-up call for {m} failed: {e}");
            std::process::exit(2);
        }
        for c in &cases {
            let r = run_case(
                &llm,
                m,
                template_lang.as_deref(),
                judge_model.as_deref(),
                c,
                &c.lang,
                true,
            )
            .await;
            eprintln!(
                "{m} {} ok={} cov={:.2} judge={:?} valid={}/{} hit={:.2} lang={:.2} ai={:?} {}ms {:?}",
                c.id,
                r.ok,
                r.coverage_kw,
                r.coverage_judge,
                r.cites_valid,
                r.cites_total,
                r.cite_hit,
                r.lang_ok,
                r.ai_recall,
                r.summary_ms,
                r.error
            );
            rows.push(r);
            if cross {
                let other = if c.lang == "vi" { "en" } else { "vi" };
                let r = run_case(&llm, m, template_lang.as_deref(), None, c, other, false).await;
                eprintln!(
                    "{m} {} -> {other} lang={:.2} valid={}/{}",
                    c.id, r.lang_ok, r.cites_valid, r.cites_total
                );
                rows.push(r);
            }
        }
    }
    let tag = template_lang
        .map(|t| format!("-tpl{t}"))
        .unwrap_or_default();
    std::fs::write(
        out.join(format!("rows{tag}.json")),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();

    println!(
        "| model | set | n | coverage (kw) | coverage (judge) | cite validity | grounded bullets | cite hit | output lang ok | action-item recall | summary latency (s) | tok/s |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|");
    for m in &models {
        for (set, pred) in [
            (
                "vi→vi",
                Box::new(|r: &Row| r.lang == "vi" && r.out_lang == "vi")
                    as Box<dyn Fn(&Row) -> bool>,
            ),
            (
                "en→en",
                Box::new(|r: &Row| r.lang == "en" && r.out_lang == "en"),
            ),
            ("cross", Box::new(|r: &Row| r.lang != r.out_lang)),
        ] {
            let rs: Vec<&Row> = rows.iter().filter(|r| &r.model == m && pred(r)).collect();
            if rs.is_empty() {
                continue;
            }
            let ok: Vec<&&Row> = rs.iter().filter(|r| r.ok).collect();
            let (ct, cv): (u32, u32) = ok
                .iter()
                .fold((0, 0), |a, r| (a.0 + r.cites_total, a.1 + r.cites_valid));
            let judge = ok
                .iter()
                .filter_map(|r| r.coverage_judge)
                .collect::<Vec<_>>();
            let ai = ok.iter().filter_map(|r| r.ai_recall).collect::<Vec<_>>();
            let fmt = |x: f64| {
                if x.is_nan() {
                    "–".to_string()
                } else {
                    format!("{x:.2}")
                }
            };
            let same = set != "cross";
            println!(
                "| {m} | {set} | {}/{} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {:.0} |",
                ok.len(),
                rs.len(),
                if same {
                    fmt(mean(ok.iter().map(|r| r.coverage_kw)))
                } else {
                    "–".into()
                },
                if judge.is_empty() {
                    "–".into()
                } else {
                    fmt(mean(judge.into_iter()))
                },
                fmt(if ct == 0 {
                    f64::NAN
                } else {
                    cv as f64 / ct as f64
                }),
                fmt(mean(ok.iter().map(|r| r.grounded))),
                if same {
                    fmt(mean(ok.iter().map(|r| r.cite_hit)))
                } else {
                    "–".into()
                },
                fmt(mean(ok.iter().map(|r| r.lang_ok))),
                if ai.is_empty() {
                    "–".into()
                } else {
                    fmt(mean(ai.into_iter()))
                },
                mean(ok.iter().map(|r| r.summary_ms as f64)) / 1000.0,
                mean(ok.iter().map(|r| r.tok_per_s)),
            );
        }
    }
    let failed: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            r.error
                .as_ref()
                .map(|e| format!("{} {}->{}: {e}", r.model, r.case, r.out_lang))
        })
        .collect();
    if !failed.is_empty() {
        eprintln!(
            "ERROR: {} run(s) failed; numbers above exclude them:",
            failed.len()
        );
        for f in &failed {
            eprintln!("  {f}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(fold("Đà Lạt"), "da lat");
        assert_eq!(detect_lang("Ngân sách marketing là 350 triệu đồng"), "vi");
        assert_eq!(
            detect_lang("The budget is 350 million VND for Đà Lạt"),
            "en"
        );
        assert_eq!(
            groups_matched(
                "Thuê micro-KOL, 50.000 view",
                &[vec!["micro".into()], vec!["50.000".into()]]
            ),
            2
        );
    }
}

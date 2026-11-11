//! Real Ollama (ignored by default): every built-in recipe on a fixture meeting whose transcript
//! looks like the Vietnamese Zipformer's output (ALL CAPS, no punctuation, numbers spelled out),
//! through the core's job loop. Prints each answer, its citations and the time it took.
//!
//! ```text
//! cargo test -p almond-core --test recipes_ollama -- --ignored --nocapture
//! ```
//! Env: `ALMOND_RECIPES_OUT` (directory for the answers as Markdown; default: temp dir),
//! `ALMOND_RECIPES_LANG` (`vi` default, or `en`). Uses the Ollama at `OLLAMA_HOST` /
//! `http://127.0.0.1:11434` and the default model slot (gemma4:12b on a GPU).

mod common;

use almond_types::artifact::body::{RecipeOutput, Segment, Transcript};
use almond_types::event::EventBody;
use almond_types::ids::SpeakerId;
use almond_types::meeting::AudioChannel;
use almond_types::mock::fixtures;
use almond_types::store::Store;
use almond_types::{ArtifactDraft, Id, LangTag, Provenance};
use common::open_core;
use serde_json::json;
use std::time::{Duration, Instant};

/// (start s, end s, speaker, text). "me" is the mic; S1 = Lan (PM), S2 = David (engineer).
const LINES: &[(f32, f32, &str, &str)] = &[
    (
        0.0,
        6.5,
        "me",
        "ỜM CHÀO MỌI NGƯỜI HÔM NAY MÌNH CHỐT KẾ HOẠCH RA MẮT BẢN HAI CỦA APP NHÉ",
    ),
    (
        7.0,
        15.0,
        "S1",
        "DẠ EM LÀ LAN EM TÓM TẮT NHANH BẢN HAI CÓ BA TÍNH NĂNG CHÍNH LÀ ĐỒNG BỘ GHI CHÚ TÌM KIẾM TIẾNG VIỆT VÀ THÔNG BÁO NHẮC LỊCH",
    ),
    (
        15.5,
        24.0,
        "S1",
        "THEO KẾ HOẠCH CŨ THÌ NGÀY RA MẮT LÀ MƯỜI MỘT THÁNG MƯỜI MỘT NHƯNG ĐỘI QA BÁO CÒN HAI MƯƠI BA LỖI CHƯA SỬA",
    ),
    (22.5, 27.0, "me", "HAI MƯƠI BA LỖI À KIỂU NHƯ NHIỀU VẬY"),
    (
        27.5,
        37.0,
        "S2",
        "ANH DAVID ĐÂY TRONG ĐÓ CÓ BẢY LỖI NGHIÊM TRỌNG CHỦ YẾU LÀ ĐỒNG BỘ TRÊN ANDROID BỊ HỆ ĐIỀU HÀNH TẮT KHI TỐI ƯU HOÁ PIN",
    ),
    (
        37.5,
        44.0,
        "S2",
        "ANH NGHĨ CẦN THÊM MỘT TUẦN ĐỂ SỬA HẾT BẢY LỖI ĐÓ",
    ),
    (
        44.5,
        52.0,
        "me",
        "Ờ VẬY MÌNH DỜI NGÀY RA MẮT SANG MƯỜI TÁM THÁNG MƯỜI MỘT ĐƯỢC KHÔNG THỨ TƯ TUẦN SAU",
    ),
    (52.5, 56.0, "S1", "DẠ ĐƯỢC ANH EM THẤY HỢP LÝ"),
    (56.5, 60.0, "S2", "OK MƯỜI TÁM LÀ ỔN"),
    (
        60.5,
        66.0,
        "me",
        "CHỐT LÀ MƯỜI TÁM THÁNG MƯỜI MỘT NHÉ LAN CẬP NHẬT LẠI LỊCH GIÚP ANH",
    ),
    (
        66.5,
        72.0,
        "S1",
        "DẠ EM SẼ GỬI LỊCH MỚI CHO CẢ NHÓM TRƯỚC THỨ SÁU NÀY",
    ),
    (
        73.0,
        82.0,
        "S1",
        "VỀ MARKETING THÌ NGÂN SÁCH ĐỀ XUẤT LÀ HAI TRĂM TRIỆU ĐỒNG TĂNG MƯỜI LĂM PHẦN TRĂM SO VỚI LẦN TRƯỚC",
    ),
    (82.5, 86.0, "me", "HAI TRĂM TRIỆU THÌ CHIA THẾ NÀO"),
    (
        86.5,
        97.0,
        "S1",
        "MỘT TRĂM HAI MƯƠI TRIỆU CHO QUẢNG CÁO FACEBOOK VÀ TIKTOK CÒN TÁM MƯƠI TRIỆU CHO SỰ KIỆN RA MẮT Ở THÀNH PHỐ HỒ CHÍ MINH",
    ),
    (
        95.5,
        103.0,
        "me",
        "THÔI KIỂU NHƯ SỰ KIỆN THÌ ANH CHƯA CHẮC NÓI CHUNG LÀ ĐỂ ANH XEM LẠI PHẦN SỰ KIỆN SAU",
    ),
    (103.5, 107.0, "S1", "DẠ VÂNG"),
    (
        108.0,
        117.0,
        "S2",
        "CÒN MỘT VIỆC NỮA LÀ THÔNG BÁO ĐẨY ANH SẼ LÀM XONG PHẦN BACKEND TRƯỚC THỨ HAI NHƯNG CẦN THÊM MỘT BẠN TESTER",
    ),
    (
        117.5,
        122.0,
        "me",
        "ỪM ĐƯỢC TUYỂN THÊM MỘT TESTER BÁN THỜI GIAN NHÉ",
    ),
    (
        122.5,
        128.0,
        "S1",
        "EM ĐĂNG TIN TUYỂN DỤNG LUÔN TRONG TUẦN NÀY",
    ),
    (
        128.5,
        136.0,
        "S2",
        "À CÒN GIÁ GÓI TRẢ PHÍ MÌNH ĐÃ CHỐT CHƯA BẢN MỘT ĐANG LÀ BA MƯƠI CHÍN NGHÌN MỘT THÁNG",
    ),
    (
        136.5,
        141.0,
        "me",
        "CÁI ĐÓ ĐỂ TUẦN SAU BÀN TIẾP ANH CẦN XEM SỐ LIỆU ĐÃ",
    ),
    (
        141.5,
        149.0,
        "S1",
        "DẠ EM NHẮC THÊM LÀ NẾU ANDROID VẪN BỊ TẮT ĐỒNG BỘ THÌ CẦN HƯỚNG DẪN NGƯỜI DÙNG TẮT TỐI ƯU HOÁ PIN",
    ),
    (
        149.5,
        155.0,
        "me",
        "ĐÚNG RỒI DAVID VIẾT GIÚP ANH MỘT TRANG HƯỚNG DẪN NGẮN NHÉ",
    ),
    (155.5, 158.0, "S2", "OK ANH"),
    (
        158.5,
        166.0,
        "me",
        "VẬY TÓM LẠI RA MẮT MƯỜI TÁM THÁNG MƯỜI MỘT LAN GỬI LỊCH TRƯỚC THỨ SÁU DAVID SỬA BẢY LỖI VÀ VIẾT HƯỚNG DẪN",
    ),
    (166.5, 170.0, "me", "CẢM ƠN MỌI NGƯỜI NHÉ"),
];

fn transcript() -> Transcript {
    let segments = LINES
        .iter()
        .enumerate()
        .map(|(i, (t0, t1, spk, text))| Segment {
            idx: i as u32,
            t0_ms: (t0 * 1000.0) as u32,
            t1_ms: (t1 * 1000.0) as u32,
            channel: if *spk == "me" {
                AudioChannel::Mic
            } else {
                AudioChannel::System
            },
            speaker: Some(SpeakerId(spk.to_string())),
            lang: Some(LangTag::vi()),
            text: text.to_string(),
            confidence: Some(0.9),
            words: None,
        })
        .collect();
    Transcript {
        segments,
        speakers: vec![],
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs the real Ollama (gemma4:12b)"]
async fn builtins_on_a_fixture_meeting() {
    let lang = std::env::var("ALMOND_RECIPES_LANG").unwrap_or_else(|_| "vi".into());
    let out_dir = std::env::var_os("ALMOND_RECIPES_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("almond-recipes-ollama"));
    std::fs::create_dir_all(&out_dir).unwrap();
    let (api, dir) = open_core("recipes-ollama").await;
    let core = api.core.clone();
    let info = core.processing.refresh_capabilities(&core).await;
    assert!(
        !info.llm_models.is_empty(),
        "Ollama not available: {:?}",
        info.llm_error
    );
    eprintln!(
        "slots: {}",
        api.call("models.slots", json!({})).await.unwrap()
    );

    let mut m = fixtures::meeting(almond_types::time::now_ms() - 10 * 60_000);
    m.title = "Kế hoạch ra mắt app bản 2".into();
    m.languages = vec![LangTag::vi()];
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let t = ArtifactDraft::new(
        m.id.clone(),
        &transcript(),
        Provenance {
            processor: "transcribe.fixture".into(),
            processor_version: "0".into(),
            ..Default::default()
        },
    )
    .unwrap()
    .into_artifact(
        Id::new(),
        almond_types::time::HlcClock::new().now(),
        core.device_id().clone(),
    );
    core.store()
        .append_local(EventBody::ArtifactAdd(t.clone()))
        .unwrap();
    let view = almond_llm::TranscriptView::from_artifact(&t).unwrap();
    let stats = almond_core::recipes::stats::render(&almond_core::recipes::stats::compute(&view));
    eprintln!("statistics given to the model:\n{stats}");

    let ids = [
        "builtin:teach_back",
        "builtin:forgot_to_ask",
        "builtin:communication",
        "builtin:decisions",
        "builtin:followup_email",
    ];
    let mut report = String::new();
    for id in ids {
        let t0 = Instant::now();
        let job = api
            .call(
                "recipes.run",
                json!({ "recipe_id": id, "meeting_id": m.id, "lang": lang }),
            )
            .await
            .unwrap();
        let job_id = job["id"].as_str().unwrap().to_string();
        let out = loop {
            let found = core
                .store()
                .list_artifacts(&m.id, Some("recipe_output"))
                .unwrap()
                .into_iter()
                .find(|a| a.decode::<RecipeOutput>().is_ok_and(|o| o.recipe_id == id));
            if let Some(a) = found {
                break a;
            }
            let jobs = api.call("jobs.list", json!({})).await.unwrap();
            if let Some(j) = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|j| j["job"]["id"] == job_id.as_str())
                && j["status"] == "failed"
            {
                panic!("{id} failed: {}", j["message"]);
            }
            assert!(t0.elapsed() < Duration::from_secs(600), "{id} timed out");
            tokio::time::sleep(Duration::from_millis(500)).await;
        };
        let o: RecipeOutput = out.decode().unwrap();
        let took = t0.elapsed();
        assert!(!o.markdown.trim().is_empty());
        assert!(o.citations.iter().all(|c| c.artifact == t.id));
        let n_markers = o.markdown.matches('[').count();
        eprintln!(
            "\n===== {} ({:.1}s, model {:?}, {} citations) =====\n{}\n",
            o.recipe_name,
            took.as_secs_f32(),
            out.provenance.model,
            o.citations.len(),
            o.markdown
        );
        report.push_str(&format!(
            "# {} ({:.1}s, {}, {} citations, {} markers)\n\n{}\n\nCitations: {}\n\n",
            o.recipe_name,
            took.as_secs_f32(),
            out.provenance.model.clone().unwrap_or_default(),
            o.citations.len(),
            n_markers,
            o.markdown,
            o.citations
                .iter()
                .enumerate()
                .map(|(i, c)| format!("[{}]=seg {}", i + 1, c.segment))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    let path = out_dir.join(format!("recipes_{lang}.md"));
    std::fs::write(&path, report).unwrap();
    eprintln!("written to {}", path.display());
    api.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

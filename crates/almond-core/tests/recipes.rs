//! Recipes (cookbook) through the core with a mock LLM: built-ins, save / edit / edit-a-built-in
//! makes a copy / hide / delete, `recipes.run` → `recipe.llm` job → `recipe_output` artifact with
//! validated citations, rerun supersedes (only the same recipe), auto-run once per new
//! transcript, and a user recipe syncing to a second core.

mod common;

use almond_core::Api;
use almond_types::artifact::body::RecipeOutput;
use almond_types::event::EventBody;
use almond_types::llm::{ChatRequest, ChatResponse, LlmModelInfo, LlmProvider};
use almond_types::mock::fixtures;
use almond_types::store::Store;
use almond_types::{Artifact, Error, Id, Meeting, Result};
use async_trait::async_trait;
use common::open_core;
use futures::stream::BoxStream;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Answers free-text requests (recipes) with Markdown citing transcript lines (one bogus), and
/// structured requests (summary, title, ...) with `{}`. Remembers the recipe prompts it saw.
#[derive(Default)]
struct RecipeMockLlm {
    prompts: Mutex<Vec<String>>,
}

#[async_trait]
impl LlmProvider for RecipeMockLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn health(&self) -> Result<()> {
        Ok(())
    }
    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        Ok(vec![LlmModelInfo {
            name: "mock:latest".into(),
            digest: Some("d1".into()),
            size_bytes: Some(1),
            context_length: Some(32_768),
        }])
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let content = if req.json_schema.is_some() {
            "{}".to_string()
        } else {
            let user = req.messages.last().unwrap().content.clone();
            self.prompts.lock().unwrap().push(user);
            "```markdown\n## Budget\n- Marketing budget is 200 triệu [1].\n- Hire two backend \
             engineers [2, 3], plus a made-up line [99].\n- Lan sends the plan [3].\n```"
                .to_string()
        };
        Ok(ChatResponse {
            content,
            model: self.list_models().await?.remove(0),
            prompt_tokens: Some(10),
            completion_tokens: Some(10),
        })
    }
    async fn chat_stream(&self, _: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        Err(Error::Unsupported("stream".into()))
    }
}

async fn call(api: &Api, m: &str, p: Value) -> Value {
    match api.call(m, p.clone()).await {
        Ok(v) => v,
        Err(e) => panic!("{m}({p}) failed: {e}"),
    }
}

async fn with_llm(name: &str) -> (Api, std::path::PathBuf, Arc<RecipeMockLlm>) {
    let (api, dir) = open_core(name).await;
    let llm = Arc::new(RecipeMockLlm::default());
    api.core
        .processing
        .set_llm_provider(&api.core, llm.clone() as Arc<dyn LlmProvider>)
        .await;
    (api, dir, llm)
}

/// A meeting with the fixture transcript (4 segments: "me", S1, S2, "me"), written by this
/// device as `transcribe.local` would.
fn meeting_with_transcript(api: &Api) -> (Meeting, Artifact) {
    let core = &api.core;
    let m = fixtures::meeting(almond_types::time::now_ms() - 30 * 60_000);
    core.store()
        .append_local(EventBody::MeetingUpsert(m.clone()))
        .unwrap();
    let t = fixtures::transcript(&m.id, core.device_id());
    core.store()
        .append_local(EventBody::ArtifactAdd(t.clone()))
        .unwrap();
    core.processing.kick();
    (m, t)
}

fn outputs(api: &Api, m: &Id) -> Vec<Artifact> {
    api.core
        .store()
        .list_artifacts(m, Some("recipe_output"))
        .unwrap()
}

async fn wait_outputs(api: &Api, m: &Id, n: usize) -> Vec<Artifact> {
    let t0 = Instant::now();
    loop {
        let v = outputs(api, m);
        if v.len() >= n {
            return v;
        }
        if t0.elapsed() > Duration::from_secs(30) {
            let jobs = api.call("jobs.list", json!({})).await.unwrap();
            panic!("{} recipe outputs, want {n}; jobs: {jobs:#}", v.len());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn recipe_jobs(api: &Api, m: &Id) -> usize {
    api.core
        .processing
        .runner()
        .job_states()
        .unwrap()
        .values()
        .filter(|s| s.job.processor == "recipe.llm" && s.job.meeting_id == *m)
        .count()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookbook_crud_and_builtins() {
    let (api, dir) = open_core("recipes-crud").await;
    let list = call(&api, "recipes.list", json!({})).await;
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "builtin:teach_back",
            "builtin:forgot_to_ask",
            "builtin:communication",
            "builtin:decisions",
            "builtin:followup_email"
        ]
    );
    for r in list.as_array().unwrap() {
        assert_eq!(r["builtin"], true);
        assert_eq!(r["hidden"], false);
        assert!(!r["prompt"].as_str().unwrap().is_empty());
        assert!(r["icon"].is_string());
    }

    // Create, then edit a user recipe (same id).
    let mine = call(
        &api,
        "recipes.save",
        json!({ "name": "  Risks  ", "prompt": "List the risks.", "icon": "⚠️" }),
    )
    .await;
    let mine_id = mine["id"].as_str().unwrap().to_string();
    assert_eq!(mine["name"], "Risks");
    assert_eq!(mine["builtin"], false);
    let edited = call(
        &api,
        "recipes.save",
        json!({ "id": mine_id, "prompt": "List the risks and owners.", "auto_run": true }),
    )
    .await;
    assert_eq!(edited["id"], mine_id.as_str());
    assert_eq!(edited["prompt"], "List the risks and owners.");
    assert_eq!(edited["auto_run"], true);
    assert_eq!(edited["name"], "Risks", "unchanged fields are kept");

    // Validation.
    let e = api
        .call("recipes.save", json!({ "name": "No prompt" }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32602, "{e}");

    // Editing a built-in's text saves a copy; the built-in is unchanged.
    let copy = call(
        &api,
        "recipes.save",
        json!({ "id": "builtin:decisions", "prompt": "Decisions only, as a numbered list." }),
    )
    .await;
    let copy_id = copy["id"].as_str().unwrap();
    assert_ne!(copy_id, "builtin:decisions");
    assert_eq!(copy["builtin"], false);
    assert_eq!(copy["name"], "Decisions & owners");
    // Hiding / auto-run on a built-in are preferences on the built-in itself.
    let hidden = call(
        &api,
        "recipes.save",
        json!({ "id": "builtin:teach_back", "hidden": true, "auto_run": true }),
    )
    .await;
    assert_eq!(hidden["id"], "builtin:teach_back");
    let list = call(&api, "recipes.list", json!({})).await;
    let get = |id: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == id)
            .cloned()
            .unwrap()
    };
    assert_eq!(get("builtin:teach_back")["hidden"], true);
    assert_eq!(get("builtin:teach_back")["auto_run"], true);
    assert_eq!(
        get("builtin:decisions")["prompt"]
            .as_str()
            .unwrap()
            .contains("Markdown table"),
        true
    );
    assert_eq!(
        get(copy_id)["prompt"],
        "Decisions only, as a numbered list."
    );
    assert_eq!(list.as_array().unwrap().len(), 7);

    // Built-ins can't be deleted; user recipes can.
    let e = api
        .call("recipes.delete", json!({ "id": "builtin:teach_back" }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32602);
    call(&api, "recipes.delete", json!({ "id": mine_id })).await;
    let e = api
        .call("recipes.delete", json!({ "id": mine_id }))
        .await
        .unwrap_err();
    assert_eq!(e.code, -32004);
    assert_eq!(
        call(&api, "recipes.list", json!({}))
            .await
            .as_array()
            .unwrap()
            .len(),
        6
    );

    api.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn run_produces_cited_output_and_rerun_supersedes() {
    let (api, dir, llm) = with_llm("recipes-run").await;
    let core = api.core.clone();

    // No transcript yet: a clear error.
    let m0 = fixtures::meeting(almond_types::time::now_ms());
    core.store()
        .append_local(EventBody::MeetingUpsert(m0.clone()))
        .unwrap();
    let e = api
        .call(
            "recipes.run",
            json!({ "recipe_id": "builtin:communication", "meeting_id": m0.id }),
        )
        .await
        .unwrap_err();
    assert!(e.message.contains("no transcript"), "{e}");

    let (m, t) = meeting_with_transcript(&api);
    let job = call(
        &api,
        "recipes.run",
        json!({ "recipe_id": "builtin:communication", "meeting_id": m.id, "extra": "focus on hiring", "lang": "en" }),
    )
    .await;
    assert_eq!(job["processor"], "recipe.llm");
    assert_eq!(job["inputs"], json!([t.id]));
    assert_eq!(job["params"]["recipe"]["id"], "builtin:communication");
    assert_eq!(job["params"]["lang"], "en");

    let outs = wait_outputs(&api, &m.id, 1).await;
    let a = &outs[0];
    assert_eq!(a.provenance.processor, "recipe.llm/builtin:communication");
    assert_eq!(a.provenance.inputs, vec![t.id.clone()]);
    assert_eq!(a.provenance.model.as_deref(), Some("mock:mock:latest@d1"));
    assert!(a.provenance.params_hash.is_some());
    assert_eq!(a.supersedes, None);
    let o: RecipeOutput = a.decode().unwrap();
    assert_eq!(o.recipe_id, "builtin:communication");
    assert_eq!(o.recipe_name, "How did I communicate?");
    // Fence stripped; [1] [2, 3] [99] [3] -> [1] [2][3] (dropped) [3]; numbering by first use.
    // "How did I communicate?" starts with the statistics table rendered by the code.
    let (table, answer) = o.markdown.split_once("\n\n## Budget").unwrap();
    assert!(table.starts_with("## Statistics\n"), "{table}");
    assert!(
        table.contains("| **You** | 00:09 | 50% | 2 | 00:04 |"),
        "{table}"
    );
    assert_eq!(
        format!("## Budget{answer}"),
        "## Budget\n- Marketing budget is 200 triệu [1].\n- Hire two backend engineers [2][3], \
         plus a made-up line.\n- Lan sends the plan [3]."
    );
    let segs: Vec<u32> = o.citations.iter().map(|c| c.segment).collect();
    assert_eq!(segs, vec![1, 2, 3]);
    assert!(o.citations.iter().all(|c| c.artifact == t.id));
    assert_eq!(o.citations[0].time_ms, Some((5000, 9500)));

    // The prompt carried the recipe, the extra instruction, the language and the statistics
    // computed in code ("me" spoke segments 0 and 3: 9 s of 18 s = 50%).
    let prompt = llm.prompts.lock().unwrap().last().unwrap().clone();
    assert!(
        prompt.contains("Recipe \"How did I communicate?\""),
        "{prompt}"
    );
    assert!(prompt.contains("Additional instruction for this run: focus on hiring"));
    assert!(prompt.contains("Write in English"));
    assert!(prompt.contains("[1] 00:05 S1: Ngân sách marketing"));
    assert!(
        prompt.contains("| me (the user) | 00:09 | 50% | 2 |"),
        "{prompt}"
    );

    // recipes.outputs
    let listed = call(&api, "recipes.outputs", json!({ "meeting_id": m.id })).await;
    assert_eq!(listed.as_array().unwrap().len(), 1);

    // A different recipe does not supersede it; a rerun of the same one does.
    call(
        &api,
        "recipes.run",
        json!({ "recipe_id": "builtin:decisions", "meeting_id": m.id }),
    )
    .await;
    wait_outputs(&api, &m.id, 2).await;
    call(
        &api,
        "recipes.run",
        json!({ "recipe_id": "builtin:communication", "meeting_id": m.id }),
    )
    .await;
    let all = wait_outputs(&api, &m.id, 3).await;
    let first = a.id.clone();
    let decisions = all
        .iter()
        .find(|x| x.provenance.processor == "recipe.llm/builtin:decisions")
        .unwrap();
    assert_eq!(decisions.supersedes, None);
    let rerun = all
        .iter()
        .find(|x| x.id != first && x.provenance.processor == "recipe.llm/builtin:communication")
        .unwrap();
    assert_eq!(rerun.supersedes, Some(first.clone()));
    let listed: Vec<Artifact> =
        serde_json::from_value(call(&api, "recipes.outputs", json!({ "meeting_id": m.id })).await)
            .unwrap();
    let mut ids: Vec<&Id> = listed.iter().map(|x| &x.id).collect();
    ids.sort();
    let mut want = vec![&rerun.id, &decisions.id];
    want.sort();
    assert_eq!(ids, want, "one live output per recipe");

    // Unknown recipe / meeting.
    let e = api
        .call(
            "recipes.run",
            json!({ "recipe_id": "nope", "meeting_id": m.id }),
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, -32004);

    api.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_run_once_per_new_transcript() {
    let (api, dir, _llm) = with_llm("recipes-auto").await;
    call(
        &api,
        "recipes.save",
        json!({ "id": "builtin:decisions", "auto_run": true }),
    )
    .await;
    let mine = call(
        &api,
        "recipes.save",
        json!({ "name": "Risks", "prompt": "List the risks.", "auto_run": true }),
    )
    .await;

    let (m, t) = meeting_with_transcript(&api);
    let outs = wait_outputs(&api, &m.id, 2).await;
    let mut got: Vec<String> = outs
        .iter()
        .map(|a| a.decode::<RecipeOutput>().unwrap().recipe_id)
        .collect();
    got.sort();
    let mut want = vec![
        "builtin:decisions".to_string(),
        mine["id"].as_str().unwrap().to_string(),
    ];
    want.sort();
    assert_eq!(got, want);
    assert_eq!(recipe_jobs(&api, &m.id), 2);

    // Seeing the same transcript again (a second notification, the startup sweep) requests
    // nothing new.
    let again = almond_core::recipes::auto_run(&api.core, &t).unwrap();
    assert!(again.is_empty());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(recipe_jobs(&api, &m.id), 2);
    assert_eq!(outputs(&api, &m.id).len(), 2);

    // A transcript written by another device is left to that device.
    let m2 = fixtures::meeting(almond_types::time::now_ms());
    api.core
        .store()
        .append_local(EventBody::MeetingUpsert(m2.clone()))
        .unwrap();
    let other = fixtures::transcript(&m2.id, &almond_types::DeviceId("other-device".into()));
    assert!(
        almond_core::recipes::auto_run(&api.core, &other)
            .unwrap()
            .is_empty()
    );

    api.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn user_recipe_syncs_to_a_second_core() {
    let (a, dir_a) = open_core("recipes-sync-a").await;
    let (b, dir_b) = open_core("recipes-sync-b").await;
    let mine = call(
        &a,
        "recipes.save",
        json!({ "name": "Standup recap", "prompt": "Who is blocked?", "icon": "🧍" }),
    )
    .await;
    call(
        &a,
        "recipes.save",
        json!({ "id": "builtin:followup_email", "hidden": true }),
    )
    .await;

    let begin = call(&a, "sync.pairBegin", json!({})).await;
    call(
        &b,
        "sync.pairAccept",
        json!({ "qr_payload": begin["qr_payload"] }),
    )
    .await;
    let ida = a.core.device_id().0.clone();
    let t0 = Instant::now();
    loop {
        let _ = b.call("sync.now", json!({ "peers": [ida] })).await;
        let list = call(&b, "recipes.list", json!({})).await;
        let got = list
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == mine["id"])
            .cloned();
        let hidden = list
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "builtin:followup_email" && r["hidden"] == true);
        if let Some(r) = got
            && hidden
        {
            assert_eq!(r["name"], "Standup recap");
            assert_eq!(r["prompt"], "Who is blocked?");
            assert_eq!(r["icon"], "🧍");
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "recipe did not sync: {list:#}"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // A delete syncs too.
    call(&a, "recipes.delete", json!({ "id": mine["id"] })).await;
    let t0 = Instant::now();
    loop {
        let _ = b.call("sync.now", json!({ "peers": [ida] })).await;
        let list = call(&b, "recipes.list", json!({})).await;
        if !list
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == mine["id"])
        {
            break;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "delete did not sync"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    a.core.shutdown().await;
    b.core.shutdown().await;
    let _ = std::fs::remove_dir_all(dir_a);
    let _ = std::fs::remove_dir_all(dir_b);
}

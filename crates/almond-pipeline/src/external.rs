//! External processors: any program speaking JSON-RPC 2.0 (docs/API.md "External processors").
//!
//! - **stdio**: the host spawns the program; one JSON message per line on stdin/stdout
//!   (stderr is passed through for logs). Supports `processor.progress` notifications
//!   (processor → host) and sends `processor.cancel` `{job_id}` (host → processor) on cancel.
//! - **HTTP**: `POST <url>` with a JSON-RPC request body, JSON-RPC response body. No progress.
//!
//! Methods: `processor.describe` → `ProcessorDescriptor[]`;
//! `processor.run` `{processor, job_id, meeting, params, inputs}` → `ArtifactDraft[]`.
//! Error codes: `-32004` not found, `-32005` unsupported, `-32001` auth required,
//! `-32602` invalid params; anything else maps to `Error::Other`.
//!
//! See `examples/external_processor.py` for a stdlib-only Python implementation.

use almond_types::pipeline::{JobCtx, ProcessorDescriptor, ProgressFn};
use almond_types::{Artifact, ArtifactDraft, Error, Id, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;

/// Default timeout for `processor.run` (describe uses 30 s).
pub const DEFAULT_RUN_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// A JSON-RPC connection to an external processor program.
#[async_trait]
pub trait RpcTransport: Send + Sync {
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value>;
    async fn notify(&self, method: &str, params: Value) -> Result<()>;
    /// Route `processor.progress` for `job` to `f` (None = stop routing).
    fn route_progress(&self, job: &Id, f: Option<ProgressFn>);
    fn describe_endpoint(&self) -> String;
}

/// Map a JSON-RPC error object to an [`Error`].
pub fn rpc_error(e: &Value) -> Error {
    let code = e["code"].as_i64().unwrap_or(0);
    let msg = e["message"]
        .as_str()
        .unwrap_or("external processor error")
        .to_string();
    match code {
        -32004 => Error::NotFound(msg),
        -32005 => Error::Unsupported(msg),
        -32001 => Error::AuthRequired(msg),
        -32602 | -32600 | -32700 => Error::Invalid(msg),
        -32601 => Error::Unsupported(format!("method not found: {msg}")),
        _ => Error::Other(format!("external processor error {code}: {msg}")),
    }
}

// ---------------------------------------------------------------------------- stdio

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
type ProgressRoutes = Arc<Mutex<HashMap<Id, ProgressFn>>>;

/// JSON-RPC over a child process's stdin/stdout (newline-delimited JSON).
pub struct StdioRpc {
    label: String,
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    progress: ProgressRoutes,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    _child: tokio::sync::Mutex<Child>,
}

impl StdioRpc {
    /// Spawn `program args...`. The child is killed when this value is dropped.
    pub fn spawn(program: &str, args: &[String]) -> Result<Arc<Self>> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Other(format!("spawn {program}: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let pending: Pending = Arc::default();
        let progress: ProgressRoutes = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let (p2, pr2, a2) = (pending.clone(), progress.clone(), alive.clone());
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
                    continue; // tolerate stray prints
                };
                handle_incoming(&msg, &p2, &pr2);
            }
            a2.store(false, Ordering::SeqCst);
            for (_, tx) in p2.lock().unwrap().drain() {
                let _ = tx.send(Err(Error::Network("external processor exited".into())));
            }
        });
        Ok(Arc::new(StdioRpc {
            label: format!("stdio:{program} {}", args.join(" ")),
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            progress,
            next_id: AtomicU64::new(1),
            alive,
            _child: tokio::sync::Mutex::new(child),
        }))
    }

    async fn send_line(&self, v: &Value) -> Result<()> {
        if !self.alive.load(Ordering::SeqCst) {
            return Err(Error::Network("external processor exited".into()));
        }
        let mut s = serde_json::to_string(v)?;
        s.push('\n');
        let mut w = self.stdin.lock().await;
        w.write_all(s.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }
}

fn handle_incoming(msg: &Value, pending: &Pending, progress: &ProgressRoutes) {
    if let Some(id) = msg.get("id").and_then(|i| i.as_u64())
        && (msg.get("result").is_some() || msg.get("error").is_some())
    {
        if let Some(tx) = pending.lock().unwrap().remove(&id) {
            let r = match msg.get("error") {
                Some(e) if !e.is_null() => Err(rpc_error(e)),
                _ => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(r);
        }
        return;
    }
    if msg["method"] == "processor.progress" {
        let p = &msg["params"];
        if let Some(job) = p["job_id"].as_str() {
            let f = progress.lock().unwrap().get(&Id(job.to_string())).cloned();
            if let Some(f) = f {
                f(
                    p["progress"].as_f64().unwrap_or(0.0) as f32,
                    p["message"].as_str().unwrap_or(""),
                );
            }
        }
    }
}

#[async_trait]
impl RpcTransport for StdioRpc {
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.send_line(&req).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(Error::Network("external processor exited".into())),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(Error::Network(format!(
                    "{method} timed out after {timeout:?} ({})",
                    self.label
                )))
            }
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send_line(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .await
    }

    fn route_progress(&self, job: &Id, f: Option<ProgressFn>) {
        let mut m = self.progress.lock().unwrap();
        match f {
            Some(f) => {
                m.insert(job.clone(), f);
            }
            None => {
                m.remove(job);
            }
        }
    }

    fn describe_endpoint(&self) -> String {
        self.label.clone()
    }
}

// ---------------------------------------------------------------------------- HTTP

/// JSON-RPC over HTTP POST (localhost services). No progress notifications.
pub struct HttpRpc {
    url: String,
    http: reqwest::Client,
    next_id: AtomicU64,
}

impl HttpRpc {
    pub fn new(url: impl Into<String>) -> Result<Arc<Self>> {
        Ok(Arc::new(HttpRpc {
            url: url.into(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(5))
                .no_proxy()
                .build()
                .map_err(Error::other)?,
            next_id: AtomicU64::new(1),
        }))
    }
}

#[async_trait]
impl RpcTransport for HttpRpc {
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let resp = self
            .http
            .post(&self.url)
            .timeout(timeout)
            .json(&req)
            .send()
            .await
            .map_err(|e| Error::Network(format!("{}: {e}", self.url)))?;
        let v: Value = resp
            .json()
            .await
            .map_err(|e| Error::Network(format!("{}: bad JSON-RPC response: {e}", self.url)))?;
        match v.get("error") {
            Some(e) if !e.is_null() => Err(rpc_error(e)),
            _ => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> {
        let _ = self
            .http
            .post(&self.url)
            .timeout(Duration::from_secs(5))
            .json(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
            .send()
            .await;
        Ok(())
    }

    fn route_progress(&self, _: &Id, _: Option<ProgressFn>) {}

    fn describe_endpoint(&self) -> String {
        format!("http:{}", self.url)
    }
}

// ---------------------------------------------------------------------------- processor

/// One processor exposed by an external program.
pub struct ExternalProcessor {
    desc: ProcessorDescriptor,
    rpc: Arc<dyn RpcTransport>,
    pub run_timeout: Duration,
}

impl ExternalProcessor {
    pub fn endpoint(&self) -> String {
        self.rpc.describe_endpoint()
    }
}

/// Ask the program which processors it implements and wrap each one.
pub async fn describe(rpc: Arc<dyn RpcTransport>) -> Result<Vec<Arc<ExternalProcessor>>> {
    let v = rpc
        .call("processor.describe", json!({}), Duration::from_secs(30))
        .await?;
    let descs: Vec<ProcessorDescriptor> = serde_json::from_value(v)
        .map_err(|e| Error::Invalid(format!("processor.describe returned bad descriptors: {e}")))?;
    Ok(descs
        .into_iter()
        .map(|desc| {
            Arc::new(ExternalProcessor {
                desc,
                rpc: rpc.clone(),
                run_timeout: DEFAULT_RUN_TIMEOUT,
            })
        })
        .collect())
}

/// Spawn `program args...` and return its processors (`pipeline.registerExternal{command}`).
pub async fn spawn_stdio(program: &str, args: &[String]) -> Result<Vec<Arc<ExternalProcessor>>> {
    describe(StdioRpc::spawn(program, args)?).await
}

/// Connect to a JSON-RPC HTTP endpoint (`pipeline.registerExternal{url}`).
pub async fn connect_http(url: &str) -> Result<Vec<Arc<ExternalProcessor>>> {
    describe(HttpRpc::new(url)?).await
}

#[async_trait]
impl almond_types::pipeline::Processor for ExternalProcessor {
    fn descriptor(&self) -> ProcessorDescriptor {
        self.desc.clone()
    }

    async fn run(&self, ctx: &JobCtx, inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        ctx.check_cancel()?;
        self.rpc
            .route_progress(&ctx.job_id, Some(ctx.progress.clone()));
        let params = json!({
            "processor": self.desc.name,
            "job_id": ctx.job_id,
            "meeting": ctx.meeting,
            "params": ctx.params,
            "inputs": inputs,
        });
        let call = self.rpc.call("processor.run", params, self.run_timeout);
        tokio::pin!(call);
        let res = loop {
            tokio::select! {
                r = &mut call => break r,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    if ctx.is_cancelled() {
                        let _ = self.rpc.notify("processor.cancel", json!({"job_id": ctx.job_id})).await;
                        break Err(Error::Cancelled);
                    }
                }
            }
        };
        self.rpc.route_progress(&ctx.job_id, None);
        let drafts: Vec<ArtifactDraft> = serde_json::from_value(res?)
            .map_err(|e| Error::Invalid(format!("processor.run returned bad drafts: {e}")))?;
        drafts
            .into_iter()
            .map(|mut d| {
                if !self.desc.produces.iter().any(|k| d.kind.is(k)) {
                    return Err(Error::Invalid(format!(
                        "{} produced kind {:?} not declared in `produces`",
                        self.desc.name, d.kind.0
                    )));
                }
                d.meeting_id = ctx.meeting.id.clone();
                Ok(d)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_codes() {
        assert!(matches!(
            rpc_error(&json!({"code": -32004, "message": "x"})),
            Error::NotFound(_)
        ));
        assert!(matches!(
            rpc_error(&json!({"code": -32602, "message": "x"})),
            Error::Invalid(_)
        ));
        assert!(matches!(
            rpc_error(&json!({"code": 1, "message": "x"})),
            Error::Other(_)
        ));
    }

    #[test]
    fn routes_responses_and_progress() {
        let pending: Pending = Arc::default();
        let progress: ProgressRoutes = Arc::default();
        let (tx, mut rx) = oneshot::channel();
        pending.lock().unwrap().insert(7, tx);
        let seen = Arc::new(Mutex::new(vec![]));
        let s2 = seen.clone();
        progress.lock().unwrap().insert(
            Id("J".into()),
            Arc::new(move |f, m: &str| s2.lock().unwrap().push((f, m.to_string()))),
        );
        handle_incoming(
            &json!({"jsonrpc":"2.0","method":"processor.progress","params":{"job_id":"J","progress":0.5,"message":"half"}}),
            &pending,
            &progress,
        );
        handle_incoming(
            &json!({"jsonrpc":"2.0","id":7,"error":{"code":-32005,"message":"no gpu"}}),
            &pending,
            &progress,
        );
        assert_eq!(seen.lock().unwrap()[0], (0.5, "half".to_string()));
        assert!(matches!(rx.try_recv().unwrap(), Err(Error::Unsupported(_))));
    }
}

//! Method router: `"meetings.list"` + JSON params → JSON result. Every transport (Tauri, JSON-RPC
//! over WebSocket later, MCP later) goes through [`crate::Api::call`].
//!
//! Register typed handlers with [`Router::add`]:
//! ```ignore
//! r.add("meetings.get", |core, p: IdParams| async move { core.store().get_meeting(&p.id)... });
//! ```

use crate::Core;
use crate::error::{ApiError, ApiResult};
use futures::future::BoxFuture;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

type Handler = Arc<dyn Fn(Arc<Core>, Value) -> BoxFuture<'static, ApiResult<Value>> + Send + Sync>;

#[derive(Default, Clone)]
pub struct Router {
    handlers: BTreeMap<String, Handler>,
}

impl Router {
    /// Register a typed handler. Params `null`/missing deserialize as `{}` so `{}`-param methods
    /// can use a unit-like struct or `serde_json::Value`.
    pub fn add<P, R, F, Fut>(&mut self, method: &str, f: F)
    where
        P: DeserializeOwned + Send + 'static,
        R: Serialize + 'static,
        F: Fn(Arc<Core>, P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ApiResult<R>> + Send + 'static,
    {
        let f = Arc::new(f);
        let h: Handler = Arc::new(move |core, params| {
            let f = f.clone();
            Box::pin(async move {
                let params = if params.is_null() {
                    Value::Object(Default::default())
                } else {
                    params
                };
                let p: P = serde_json::from_value(params)?;
                let r = f(core, p).await?;
                Ok(serde_json::to_value(r)?)
            })
        });
        assert!(
            self.handlers.insert(method.to_string(), h).is_none(),
            "method registered twice: {method}"
        );
    }

    pub fn has(&self, method: &str) -> bool {
        self.handlers.contains_key(method)
    }

    pub fn methods(&self) -> impl Iterator<Item = &str> {
        self.handlers.keys().map(String::as_str)
    }

    pub(crate) async fn call(
        &self,
        core: Arc<Core>,
        method: &str,
        params: Value,
    ) -> ApiResult<Value> {
        match self.handlers.get(method) {
            Some(h) => h(core, params).await,
            None => Err(ApiError::not_implemented(method)),
        }
    }
}

/// `{}` params.
#[derive(Debug, Default, serde::Deserialize)]
pub struct NoParams {}

/// `{ id }` params.
#[derive(Debug, serde::Deserialize)]
pub struct IdParams {
    pub id: almond_types::Id,
}

//! Phase 2 module "library" — see the ownership table in lib.rs and the brief in docs/PHASE2.md.

use crate::router::Router;
use crate::{Core, Shared};
use almond_types::Result;
use std::sync::Arc;

pub struct State {}

impl State {
    pub(crate) async fn init(_shared: &Shared) -> Result<State> {
        Ok(State {})
    }

    pub(crate) async fn shutdown(&self) {}
}

pub(crate) fn register(_r: &mut Router) {}

pub(crate) fn start(_core: Arc<Core>) {}

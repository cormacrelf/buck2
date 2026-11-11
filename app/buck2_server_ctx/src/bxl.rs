/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use buck2_core::bxl::BxlFilePath;
use buck2_core::deferred::base_deferred_key::BaseDeferredKeyBxl;
use buck2_core::global_cfg_options::GlobalCfgOptions;
use buck2_util::late_binding::LateBinding;
use dice::DiceTransaction;
use dice::UserComputationData;
use dupe::Dupe;

use crate::ctx::ServerCommandContextTrait;
use crate::partial_result_dispatcher::NoPartialResult;
use crate::partial_result_dispatcher::PartialResultDispatcher;

#[async_trait]
pub trait BxlServerCommands: Send + Sync + 'static {
    async fn bxl(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<buck2_cli_proto::StdoutBytes>,
        req: buck2_cli_proto::BxlRequest,
    ) -> buck2_error::Result<buck2_cli_proto::BxlResponse>;
    async fn bxl_profile(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        partial_result_dispatcher: PartialResultDispatcher<NoPartialResult>,
        req: buck2_cli_proto::ProfileRequest,
    ) -> buck2_error::Result<buck2_cli_proto::ProfileResponse>;
    /// The key of the bxl function `bxl_label`, `path.bxl:name` as the command line gives it,
    /// called with the command line arguments `bxl_args`, and the file of the function. `buck2
    /// dev` evaluates it for the providers that the function returns.
    async fn bxl_key(
        &self,
        ctx: &dyn ServerCommandContextTrait,
        dice: &DiceTransaction,
        bxl_label: &str,
        bxl_args: &[String],
        global_cfg_options: &GlobalCfgOptions,
    ) -> buck2_error::Result<(BaseDeferredKeyBxl, BxlFilePath)>;
}

pub static BXL_SERVER_COMMANDS: LateBinding<&'static dyn BxlServerCommands> =
    LateBinding::new("BXL_SERVER_COMMANDS");

#[derive(Debug, Default)]
pub struct BxlStreamingTracker {
    streaming_output_called: AtomicBool,
}

impl BxlStreamingTracker {
    pub fn mark_as_called(&self) {
        self.streaming_output_called.store(true, Ordering::SeqCst);
    }

    pub fn was_called(&self) -> bool {
        self.streaming_output_called.load(Ordering::SeqCst)
    }
}

pub trait InitBxlStreamingTracker {
    fn init_bxl_streaming_tracker(&mut self);
}

pub trait GetBxlStreamingTracker {
    fn get_bxl_streaming_tracker(&self) -> Option<Arc<BxlStreamingTracker>>;
}

impl InitBxlStreamingTracker for UserComputationData {
    fn init_bxl_streaming_tracker(&mut self) {
        self.data.set(Arc::new(BxlStreamingTracker::default()));
    }
}

impl GetBxlStreamingTracker for UserComputationData {
    fn get_bxl_streaming_tracker(&self) -> Option<Arc<BxlStreamingTracker>> {
        self.data
            .get::<Arc<BxlStreamingTracker>>()
            .map(|v| v.dupe())
            .ok()
    }
}

/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! The local resources of a `buck2 dev` session: `DevInfo.local_resources`.
//!
//! The daemon sets them up with the code that sets up the local resources of tests, and holds
//! them across the session's requests, under the session's name (see
//! `buck2_server_ctx::dev_interest`). They are released when `buck2 dev` asks, when the session's
//! last subscription ends, or when the daemon shuts down.
//!
//! A resource is set up again only when it is new, its setup is new, or the process that holds
//! it exited. Each set-up resource has a name of its own, so `buck2 dev` can tell that its server
//! was started against an earlier one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use buck2_build_api::actions::artifact::get_artifact_fs::GetArtifactFs;
use buck2_build_api::interpreter::rule_defs::provider::builtin::local_resource_info::OwnedLocalResourceInfo;
use buck2_build_api::local_resources::local_resource_executor;
use buck2_build_api::local_resources::prepare_local_resource;
use buck2_build_api::local_resources::release_local_resources;
use buck2_build_api::local_resources::start_local_resource;
use buck2_common::events::HasEvents;
use buck2_common::kill_util::process_alive;
use buck2_common::liveliness_observer::NoopLivelinessObserver;
use buck2_common::local_resource_state::LocalResourceState;
use buck2_core::provider::label::ConfiguredProvidersLabel;
use buck2_error::ErrorTag;
use buck2_execute::digest_config::HasDigestConfig;
use buck2_server_ctx::dev_interest::SessionHeld;
use dice::CancellationContext;
use dice::DiceComputations;
use dupe::Dupe;
use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::Mutex;

/// How long a setup command may take, unless its `LocalResourceInfo` says.
const DEFAULT_SETUP_TIMEOUT: Duration = Duration::from_secs(120);

/// What identifies the setup of a resource: a resource set up otherwise is out of date.
#[derive(PartialEq, Eq)]
pub(crate) struct SetupKey {
    /// The setup command line.
    pub(crate) argv: Vec<String>,
    /// A digest of the contents of what the command line names.
    pub(crate) digest: String,
    /// `LocalResourceInfo.resource_env_vars`: the variables are made from the setup's output
    /// when it is set up.
    pub(crate) env: Vec<(String, String)>,
}

/// A resource that the session's server needs.
pub(crate) struct WantedResource {
    pub(crate) name: String,
    pub(crate) target: ConfiguredProvidersLabel,
    pub(crate) info: OwnedLocalResourceInfo,
    pub(crate) key: SetupKey,
}

/// The resources of the server, once each is set up, or not.
#[derive(Default)]
pub(crate) struct ResourcesUpdate {
    pub(crate) env: Vec<(String, String)>,
    /// The name of each resource that runs. See `DevServer.resources`.
    pub(crate) running: Vec<String>,
    /// The resources that had exited, and were set up again.
    pub(crate) exited: Vec<String>,
    pub(crate) errors: Vec<String>,
}

struct Running {
    key: SetupKey,
    state: LocalResourceState,
    /// The name of this instance of the resource.
    instance: String,
}

#[derive(Default)]
struct Inner {
    /// The session ended, or the daemon is shutting down: nothing is set up any more.
    closed: bool,
    running: BTreeMap<String, Running>,
    /// For the names of the instances.
    count: u64,
}

/// The local resources that the daemon holds for a session.
#[derive(Default)]
pub(crate) struct SessionResources {
    inner: Mutex<Inner>,
}

impl SessionResources {
    /// Whether a resource that runs has exited.
    pub(crate) async fn any_exited(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.running.values().any(|r| !alive(&r.state))
    }

    /// Make the session's resources `wanted`: release those that are no longer wanted, whose
    /// setup is new or that exited, then set up the ones that do not run.
    pub(crate) async fn update(
        &self,
        dice: &mut DiceComputations<'_>,
        cancellation: &CancellationContext,
        wanted: Vec<WantedResource>,
    ) -> buck2_error::Result<ResourcesUpdate> {
        let mut inner = self.inner.lock().await;
        if inner.closed {
            return Err(buck2_error::buck2_error!(
                ErrorTag::Input,
                "The `buck2 dev` session ended, so the daemon sets up no more local resources for it"
            ));
        }
        let mut update = ResourcesUpdate::default();

        // Release the stale ones first, so that their replacements can use what they had, such
        // as a port.
        let stale: Vec<String> = inner
            .running
            .iter()
            .filter(|(name, running)| {
                let alive = alive(&running.state);
                let wanted = wanted
                    .iter()
                    .find(|w| &w.name == *name)
                    .is_some_and(|w| w.key == running.key);
                if wanted && !alive {
                    update.exited.push((*name).clone());
                }
                !wanted || !alive
            })
            .map(|(name, _)| name.clone())
            .collect();
        let stale: Vec<LocalResourceState> = stale
            .iter()
            .filter_map(|name| inner.running.remove(name))
            .map(|running| running.state)
            .collect();
        if let Err(e) = release_local_resources(stale).await {
            tracing::warn!("Failed to release local resources of a `buck2 dev` session: {e:#}");
        }

        if wanted.iter().any(|w| !inner.running.contains_key(&w.name)) {
            let fs = dice.get_artifact_fs().await?;
            let executor = local_resource_executor(dice, &fs)?;
            let events = dice.per_transaction_data().get_dispatcher().dupe();
            let digest_config = dice.global_data().get_digest_config();
            for w in &wanted {
                if inner.running.contains_key(&w.name) {
                    continue;
                }
                let setup = async {
                    let prepared = prepare_local_resource(
                        dice,
                        (w.target.target(), w.info.dupe()),
                        &fs,
                        &executor.executor_fs(),
                        DEFAULT_SETUP_TIMEOUT,
                    )
                    .await?;
                    start_local_resource(
                        events.dupe(),
                        NoopLivelinessObserver::create(),
                        digest_config,
                        executor.dupe(),
                        prepared,
                        cancellation,
                    )
                    .await
                };
                match setup.await {
                    Ok(state) => {
                        inner.count += 1;
                        let instance = format!("{}#{}", w.name, inner.count);
                        inner.running.insert(
                            w.name.clone(),
                            Running {
                                key: SetupKey {
                                    argv: w.key.argv.clone(),
                                    digest: w.key.digest.clone(),
                                    env: w.key.env.clone(),
                                },
                                state,
                                instance,
                            },
                        );
                    }
                    Err(e) => update.errors.push(format!(
                        "`buck2 dev` could not set up local resource `{}` (`{}`): {:#}",
                        w.name, w.target, e
                    )),
                }
            }
        }

        for w in &wanted {
            if let Some(running) = inner.running.get(&w.name) {
                update.running.push(running.instance.clone());
                // The server uses the first resource that the setup reported.
                let resource = running.state.acquire_resource().await;
                update.env.extend(
                    resource
                        .as_ref()
                        .0
                        .iter()
                        .map(|var| (var.key.clone(), var.value.clone())),
                );
            }
        }
        Ok(update)
    }

    /// Release every resource, and wait until they are gone.
    pub(crate) async fn release_all(&self) -> buck2_error::Result<()> {
        let states: Vec<LocalResourceState> = {
            let mut inner = self.inner.lock().await;
            std::mem::take(&mut inner.running)
                .into_values()
                .map(|running| running.state)
                .collect()
        };
        release_local_resources(states).await
    }
}

impl SessionHeld for SessionResources {
    fn release(self: Arc<Self>) -> BoxFuture<'static, ()> {
        async move {
            self.inner.lock().await.closed = true;
            if let Err(e) = self.release_all().await {
                tracing::warn!("Failed to release local resources of a `buck2 dev` session: {e:#}");
            }
        }
        .boxed()
    }
}

/// Whether the process that holds a resource still runs. A resource without one is taken to run.
fn alive(state: &LocalResourceState) -> bool {
    state.owning_pid().is_none_or(process_alive)
}

/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! Setting up and releasing local resources: the processes that a `LocalResourceInfo` starts with
//! its setup command, such as a database or a simulator. `buck2 test` sets them up for the tests
//! that need them, and `buck2 dev` for its server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use buck2_build_signals::env::WaitingData;
use buck2_common::kill_util::try_terminate_process_gracefully;
use buck2_common::liveliness_observer::LivelinessObserver;
use buck2_common::local_resource_state::EnvironmentVariable;
use buck2_common::local_resource_state::LocalResource;
use buck2_common::local_resource_state::LocalResourceState;
use buck2_core::execution_types::executor_config::CommandExecutorConfig;
use buck2_core::execution_types::executor_config::CommandGenerationOptions;
use buck2_core::execution_types::executor_config::Executor;
use buck2_core::execution_types::executor_config::LocalExecutorOptions;
use buck2_core::execution_types::executor_config::PathSeparatorKind;
use buck2_core::fs::artifact_path_resolver::ArtifactFs;
use buck2_core::target::configured_target_label::ConfiguredTargetLabel;
use buck2_data::ReleaseLocalResourcesEnd;
use buck2_data::ReleaseLocalResourcesStart;
use buck2_data::SetupLocalResourcesEnd;
use buck2_data::SetupLocalResourcesStart;
use buck2_data::ToProtoMessage;
use buck2_error::BuckErrorContext;
use buck2_error::ErrorTag;
use buck2_error::conversion::from_any_with_tag;
use buck2_events::dispatch::EventDispatcher;
use buck2_events::dispatch::span_async_simple;
use buck2_execute::artifact::fs::ExecutorFs;
use buck2_execute::digest_config::DigestConfig;
use buck2_execute::digest_config::HasDigestConfig;
use buck2_execute::execute::cache_uploader::NoOpCacheUploader;
use buck2_execute::execute::claim::MutexClaimManager;
use buck2_execute::execute::command_executor::CommandExecutor;
use buck2_execute::execute::manager::CommandExecutionManager;
use buck2_execute::execute::prepared::NoOpCommandOptionalExecutor;
use buck2_execute::execute::prepared::PreparedCommand;
use buck2_execute::execute::request::CommandExecutionInput;
use buck2_execute::execute::request::CommandExecutionPaths;
use buck2_execute::execute::request::CommandExecutionRequest;
use buck2_execute::execute::result::CommandExecutionReport;
use buck2_execute::execute::result::CommandExecutionResult;
use buck2_execute::execute::result::CommandExecutionStatus;
use buck2_execute::execute::target::CommandExecutionTarget;
use buck2_hash::BuckMutInsertionMap;
use buck2_hash::BuckMutMap;
use buck2_hash::buck_mut_insertion_set;
use dice::CancellationContext;
use dice::DiceComputations;
use dice::UserComputationData;
use dupe::Dupe;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::actions::execute::dice_data::CommandExecutorResponse;
use crate::actions::execute::dice_data::DiceHasCommandExecutor;
use crate::actions::impls::run_action_knobs::HasRunActionKnobs;
use crate::artifact_groups::calculation::ArtifactGroupCalculation;
use crate::interpreter::rule_defs::cmd_args::CommandLineBuilder;
use crate::interpreter::rule_defs::cmd_args::SimpleCommandLineArtifactVisitor;
use crate::interpreter::rule_defs::provider::builtin::local_resource_info::OwnedLocalResourceInfo;

#[derive(Deserialize)]
/// Represents the JSON schema for output of a local resource command setup.
pub struct LocalResourcesSetupResult {
    /// Process ID which is holding local resources which were set up.
    /// This process needs to be gracefully terminated in order to release
    /// any system resources related to local resources.
    pub pid: Option<i32>,
    /// A list of resource entities, each is a mapping from a string alias (e.g. `socket_address`)
    /// to a value which represents resource. String alias is mapped to an environment variable key
    /// (which will be added to a command requiring such resource) using a `LocalResourceInfo` provider.
    pub resources: Vec<BTreeMap<String, String>>,
}

impl LocalResourcesSetupResult {
    pub fn into_state(
        self,
        resource_target: ConfiguredTargetLabel,
        provider_env_mapping: &BuckMutInsertionMap<String, String>,
    ) -> buck2_error::Result<LocalResourceState> {
        fn make_resource(
            alias_to_value: BTreeMap<String, String>,
            env_var_to_alias: &BuckMutInsertionMap<String, String>,
        ) -> buck2_error::Result<LocalResource> {
            let env_vars = env_var_to_alias
                .iter()
                .map(|(env_var, alias)| {
                    let value = alias_to_value.get(alias).ok_or_else(|| buck2_error::buck2_error!(
                        ErrorTag::LocalResourceSetup,
                        "Missing value for local resource environment variable `{}` with `{}` alias",
                        env_var,
                        alias,
                    ))?.to_owned();
                    Ok(EnvironmentVariable {
                        key: env_var.to_owned(),
                        value,
                    })
                })
                .collect::<Result<_, buck2_error::Error>>()?;
            Ok(LocalResource(env_vars))
        }
        // The pid is passed to `kill(2)` when the resources are released: 0 is our own process
        // group and a negative value is a process group or every process.
        if let Some(pid) = self.pid
            && pid <= 0
        {
            return Err(buck2_error::buck2_error!(
                ErrorTag::LocalResourceSetup,
                "Local resource setup for `{}` reported pid `{}`, which is not a process",
                resource_target,
                pid,
            ));
        }
        // A test that needs the resource waits for one to be free, so an empty pool is a hang.
        if self.resources.is_empty() {
            return Err(buck2_error::buck2_error!(
                ErrorTag::LocalResourceSetup,
                "Local resource setup for `{}` reported no resources",
                resource_target,
            ));
        }
        let specs = self
            .resources
            .into_iter()
            .map(|res| make_resource(res, provider_env_mapping))
            .collect::<Result<_, buck2_error::Error>>()?;

        Ok(LocalResourceState::new(resource_target, self.pid, specs))
    }
}

#[derive(Default)]
pub struct LocalResourceRegistry(
    pub Arc<Mutex<BuckMutMap<ConfiguredTargetLabel, buck2_error::Result<LocalResourceState>>>>,
);

impl LocalResourceRegistry {
    pub async fn release_all_resources(&self) -> buck2_error::Result<()> {
        let resources = {
            let mut lock = self.0.lock().await;
            lock.drain().flat_map(|(_, v)| v).collect::<Vec<_>>()
        };
        release_local_resources(resources).await
    }
}

/// Release `resources`: send `SIGTERM` to the process that holds each, and `SIGKILL` if it has
/// not exited after 20 seconds.
pub async fn release_local_resources(
    resources: Vec<LocalResourceState>,
) -> buck2_error::Result<()> {
    if resources.is_empty() {
        return Ok(());
    }

    let cleanup = || async move {
        let resource_futs = resources
            .into_iter()
            .filter(|s| s.owning_pid().is_some())
            .map(|s| async move {
                let pid = s.owning_pid().unwrap();
                try_terminate_process_gracefully(pid, Duration::from_secs(20))
                    .await
                    .buck_error_context(format!(
                        "Failed to kill a process with `{}` PID to release local resource `{}`",
                        pid,
                        s.source_target()
                    ))
            });

        buck2_util::future::join_all(resource_futs.collect::<Vec<_>>())
            .await
            .into_iter()
            .collect::<Result<(), _>>()?;

        Ok::<(), buck2_error::Error>(())
    };

    let start = ReleaseLocalResourcesStart {};
    let end = ReleaseLocalResourcesEnd {};

    span_async_simple(start, cleanup(), end).await?;

    Ok::<(), buck2_error::Error>(())
}

pub trait InitLocalResourceRegistry {
    fn init_local_resource_registry(&mut self);
}

pub trait HasLocalResourceRegistry {
    fn get_local_resource_registry(&self) -> buck2_error::Result<Arc<LocalResourceRegistry>>;
}

impl InitLocalResourceRegistry for UserComputationData {
    fn init_local_resource_registry(&mut self) {
        self.data.set(Arc::new(LocalResourceRegistry::default()));
    }
}

impl HasLocalResourceRegistry for DiceComputations<'_> {
    fn get_local_resource_registry(&self) -> buck2_error::Result<Arc<LocalResourceRegistry>> {
        let data = self
            .per_transaction_data()
            .data
            .get::<Arc<LocalResourceRegistry>>()
            .expect("LocalResourceRegistry should be set");

        Ok(data.dupe())
    }
}

/// The setup command of a local resource, ready to run.
pub struct PreparedLocalResourceSetupContext {
    pub target: ConfiguredTargetLabel,
    pub execution_request: CommandExecutionRequest,
    pub env_var_mapping: BuckMutInsertionMap<String, String>,
}

/// The executor that runs setup commands: local execution, on this host.
pub fn local_resource_executor(
    dice: &mut DiceComputations<'_>,
    fs: &ArtifactFs,
) -> buck2_error::Result<CommandExecutor> {
    let executor_config = CommandExecutorConfig {
        executor: Executor::Local(LocalExecutorOptions::default()),
        options: CommandGenerationOptions {
            path_separator: PathSeparatorKind::system_default(),
            output_paths_behavior: Default::default(),
            use_bazel_protocol_remote_persistent_workers: false,
            network_access: None,
        },
    };
    let CommandExecutorResponse {
        executor,
        platform,
        action_cache_checker: _,
        remote_dep_file_cache_checker: _,
        cache_uploader: _,
        output_trees_download_config: _,
        write_action_cache: _,
    } = dice.get_command_executor_from_dice(fs, &executor_config)?;
    let executor = CommandExecutor::new(
        executor,
        Arc::new(NoOpCommandOptionalExecutor {}),
        Arc::new(NoOpCommandOptionalExecutor {}),
        Arc::new(NoOpCacheUploader {}),
        None,
        fs.dupe(),
        executor_config.options,
        platform,
    );
    Ok(executor)
}

/// Build what the setup command of `provider` needs, and make the request that runs it.
pub async fn prepare_local_resource(
    dice: &mut DiceComputations<'_>,
    provider: (&ConfiguredTargetLabel, OwnedLocalResourceInfo),
    fs: &ArtifactFs,
    executor_fs: &ExecutorFs<'_>,
    default_timeout: Duration,
) -> buck2_error::Result<PreparedLocalResourceSetupContext> {
    let digest_config = dice.global_data().get_digest_config();

    let (target, provider) = provider;
    // The `'v`-branded view of the provider must not be held across an await (only the
    // `OwnedFrozen` may be), so this is scoped and re-derived below.
    let visited_inputs = {
        let info = provider.as_ref().value().as_ref();
        let mut artifact_visitor = SimpleCommandLineArtifactVisitor::new();
        info.setup_command_line()
            .visit_artifacts(&mut artifact_visitor)?;
        artifact_visitor.inputs
    };

    let inputs = dice
        .try_compute_join(visited_inputs, async |dice, group| {
            dice.ensure_artifact_group(&group).await
        })
        .await?;

    let info = provider.as_ref().value().as_ref();
    let artifact_path_mapping: BuckMutMap<_, _> = inputs
        .iter()
        .flat_map(|v| v.iter())
        .map(|(a, v)| (a, v.content_based_path_hash()))
        .collect();
    let mut cmd: Vec<String> = vec![];
    info.setup_command_line()
        .add_to_command_line(&mut CommandLineBuilder::new(
            &mut cmd,
            &artifact_path_mapping,
            executor_fs,
        ))?;

    let inputs = inputs
        .into_iter()
        .map(|group_values| CommandExecutionInput::Artifact(Box::new(group_values)))
        .collect();
    let paths = CommandExecutionPaths::new(
        inputs,
        buck_mut_insertion_set![],
        fs,
        digest_config,
        dice.per_transaction_data()
            .get_run_action_knobs()
            .action_paths_interner
            .as_ref(),
    )?;
    let mut execution_request =
        CommandExecutionRequest::new(vec![], cmd, paths, Default::default());
    execution_request =
        execution_request.with_timeout(info.setup_timeout().unwrap_or(default_timeout));
    execution_request = execution_request.with_skip_resource_control();
    Ok(PreparedLocalResourceSetupContext {
        target: target.dupe(),
        execution_request,
        env_var_mapping: info.env_var_mapping(),
    })
}

/// Run a prepared setup command, and read the resources it says it set up.
pub async fn start_local_resource(
    events: EventDispatcher,
    liveliness_observer: Arc<dyn LivelinessObserver>,
    digest_config: DigestConfig,
    executor: CommandExecutor,
    context: PreparedLocalResourceSetupContext,
    cancellation: &CancellationContext,
) -> buck2_error::Result<LocalResourceState> {
    let manager = CommandExecutionManager::new(
        Box::new(MutexClaimManager::new()),
        events.dupe(),
        liveliness_observer,
        WaitingData::new(),
    );

    let local_resource_target = LocalResourceTarget {
        target: &context.target,
    };
    let prepared_action =
        executor.prepare_action(&context.execution_request, digest_config, false)?;
    let prepared_command = PreparedCommand {
        target: &local_resource_target as _,
        request: &context.execution_request,
        prepared_action: &prepared_action,
        digest_config,
    };
    let command = executor.exec_cmd(manager, &prepared_command, cancellation);

    let start = SetupLocalResourcesStart {
        target_label: Some(context.target.as_proto()),
    };
    let end = SetupLocalResourcesEnd {};
    let execution_result = events
        .span_async(start, async move { (command.await, end) })
        .await;

    let CommandExecutionResult {
        outputs: _,
        report:
            CommandExecutionReport {
                std_streams,
                exit_code,
                status,
                ..
            },
        ..
    } = execution_result;

    let std_streams = std_streams
        .into_bytes()
        .await
        .buck_error_context("Error accessing setup local resource output")?;

    match status {
        CommandExecutionStatus::Success { .. } => {}
        CommandExecutionStatus::Failure { .. } | CommandExecutionStatus::WorkerFailure { .. } => {
            return Err(buck2_error::buck2_error!(
                ErrorTag::LocalResourceSetup,
                "Local resource setup command failed with `{}` exit code, stdout:\n{}\nstderr:\n{}\n",
                exit_code.unwrap_or(1),
                String::from_utf8_lossy(&std_streams.stdout),
                String::from_utf8_lossy(&std_streams.stderr),
            ));
        }
        CommandExecutionStatus::TimedOut { duration, .. } => {
            return Err(buck2_error::buck2_error!(
                ErrorTag::LocalResourceSetup,
                "Local resource setup command timed out after `{}s`, stdout:\n{}\nstderr:\n{}\n",
                duration.as_secs(),
                String::from_utf8_lossy(&std_streams.stdout),
                String::from_utf8_lossy(&std_streams.stderr),
            ));
        }
        CommandExecutionStatus::Error { error, .. } => {
            return Err(error);
        }
        CommandExecutionStatus::Cancelled { .. } => {
            return Err(buck2_error::buck2_error!(
                ErrorTag::LocalResourceSetup,
                "Local resource setup command cancelled"
            ));
        }
    };

    let string_content = String::from_utf8_lossy(&std_streams.stdout);
    let data: LocalResourcesSetupResult = serde_json::from_str(&string_content)
        // .buck_error_context("Error parsing local resource setup command output")
        .map_err(|e| from_any_with_tag(e, ErrorTag::LocalResourceSetup))?;
    let state = data.into_state(context.target.dupe(), &context.env_var_mapping)?;

    Ok(state)
}

#[derive(Debug)]
struct LocalResourceTarget<'a> {
    target: &'a ConfiguredTargetLabel,
}

impl CommandExecutionTarget for LocalResourceTarget<'_> {
    fn re_action_key(&self) -> String {
        String::new()
    }

    fn re_affinity_key(&self) -> String {
        String::new()
    }

    fn as_proto_action_key(&self) -> buck2_data::ActionKey {
        buck2_data::ActionKey {
            id: Default::default(),
            owner: Some(buck2_data::action_key::Owner::LocalResourceSetup(
                self.target.as_proto(),
            )),
            key: Default::default(),
        }
    }

    fn as_proto_action_name(&self) -> buck2_data::ActionName {
        buck2_data::ActionName {
            category: "setup_local_resource".to_owned(),
            identifier: "".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use buck2_common::local_resource_state::EnvironmentVariable;
    use buck2_common::local_resource_state::LocalResource;
    use buck2_core::configuration::data::ConfigurationData;
    use buck2_core::target::configured_target_label::ConfiguredTargetLabel;
    use buck2_hash::buck_mut_insertion_map;

    use super::LocalResourcesSetupResult;

    #[tokio::test]
    async fn test_into_state() -> buck2_error::Result<()> {
        let setup_result = LocalResourcesSetupResult {
            pid: Some(42),
            resources: vec![
                BTreeMap::from([
                    ("socket_address".to_owned(), "foo".to_owned()),
                    ("named_pipe".to_owned(), "bar".to_owned()),
                ]),
                BTreeMap::from([
                    ("socket_address".to_owned(), "baz".to_owned()),
                    ("something_else".to_owned(), "boo".to_owned()),
                ]),
                BTreeMap::from([("socket_address".to_owned(), "qux".to_owned())]),
            ],
        };
        let target =
            ConfiguredTargetLabel::testing_parse("foo//bar:baz", ConfigurationData::testing_new());
        let provider_env_mapping = buck_mut_insertion_map! {
            "ENV_SOCKET".to_owned() => "socket_address".to_owned(),
        };
        let state = setup_result.into_state(target, &provider_env_mapping)?;
        assert_eq!(state.owning_pid(), Some(42));
        let holder1 = state.acquire_resource().await;
        let holder2 = state.acquire_resource().await;
        let holder3 = state.acquire_resource().await;
        assert_eq!(
            holder1.as_ref(),
            &LocalResource(vec![EnvironmentVariable {
                key: "ENV_SOCKET".to_owned(),
                value: "foo".to_owned()
            },])
        );
        assert_eq!(
            holder2.as_ref(),
            &LocalResource(vec![EnvironmentVariable {
                key: "ENV_SOCKET".to_owned(),
                value: "baz".to_owned()
            }])
        );
        assert_eq!(
            holder3.as_ref(),
            &LocalResource(vec![EnvironmentVariable {
                key: "ENV_SOCKET".to_owned(),
                value: "qux".to_owned()
            }])
        );
        Ok(())
    }

    /// `pid` is passed to `kill(2)` when the resources are released, so only a process id is
    /// accepted.
    #[test]
    fn test_non_positive_pid_is_rejected() {
        for pid in [0, -1] {
            let setup_result = LocalResourcesSetupResult {
                pid: Some(pid),
                resources: vec![BTreeMap::from([(
                    "socket_address".to_owned(),
                    "foo".to_owned(),
                )])],
            };
            let target = ConfiguredTargetLabel::testing_parse(
                "foo//bar:baz",
                ConfigurationData::testing_new(),
            );
            let provider_env_mapping = buck_mut_insertion_map! {
                "ENV_SOCKET".to_owned() => "socket_address".to_owned(),
            };
            let err = setup_result
                .into_state(target, &provider_env_mapping)
                .unwrap_err();
            assert!(
                format!("{err:#}").contains("which is not a process"),
                "{err:#}"
            );
        }
    }

    /// A setup that reports no resources is a setup failure, not a pool to wait on.
    #[test]
    fn test_empty_resource_list_is_rejected() {
        let setup_result = LocalResourcesSetupResult {
            pid: None,
            resources: vec![],
        };
        let target =
            ConfiguredTargetLabel::testing_parse("foo//bar:baz", ConfigurationData::testing_new());
        let provider_env_mapping = buck_mut_insertion_map! {
            "ENV_SOCKET".to_owned() => "socket_address".to_owned(),
        };
        let err = setup_result
            .into_state(target, &provider_env_mapping)
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("reported no resources"),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn test_missing_value() -> buck2_error::Result<()> {
        let setup_result = LocalResourcesSetupResult {
            pid: Some(42),
            resources: vec![
                BTreeMap::from([("socket_address".to_owned(), "foo".to_owned())]),
                BTreeMap::from([("something_else".to_owned(), "bar".to_owned())]),
            ],
        };
        let target =
            ConfiguredTargetLabel::testing_parse("foo//bar:baz", ConfigurationData::testing_new());
        let provider_env_mapping = buck_mut_insertion_map! {
            "ENV_SOCKET".to_owned() => "socket_address".to_owned(),
        };
        let result = setup_result.into_state(target, &provider_env_mapping);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("Missing value for local resource environment variable `ENV_SOCKET` with `socket_address` alias"));
        Ok(())
    }
}

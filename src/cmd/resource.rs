// This file is part of the terraform-provider-generic project
//
// Copyright (C) ANEO, 2024-2024. All rights reserved.
//
// Licensed under the Apache License, Version 2.0 (the "License")
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

use async_trait::async_trait;
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};

use tf_provider::value::{Value, ValueEmpty, ValueList, ValueMap, ValueNumber, ValueString};
use tf_provider::{schema::Schema, AttributePath, Diagnostics, Resource};

use crate::connection::Connection;
use crate::utils::{WithCmd, WithEnv, WithNormalize, WithSchema};

use super::state::{ResourceState, StateUpdate};
use super::{prepare_envs, with_env};

#[derive(Debug, Default)]
pub struct GenericCmdResource<T: Connection> {
    pub(super) connect: T,
}

impl<T: Connection> GenericCmdResource<T> {
    pub fn new(connect: T) -> Self {
        Self { connect }
    }
}

#[async_trait]
impl<T> Resource for GenericCmdResource<T>
where
    T: Connection,
    T: Debug,
    T: Clone,
{
    type State<'a> = ResourceState<'a, T>;
    type PrivateState<'a> = ValueNumber;
    type ProviderMetaState<'a> = ValueEmpty;

    fn schema(&self, _diags: &mut Diagnostics) -> Option<Schema> {
        Some(ResourceState::<T>::schema())
    }

    async fn validate<'a>(&self, diags: &mut Diagnostics, config: Self::State<'a>) -> Option<()> {
        self.validate(diags, &config, Default::default()).await;

        if diags.errors.is_empty() {
            Some(())
        } else {
            None
        }
    }

    async fn read<'a>(
        &self,
        diags: &mut Diagnostics,
        state: Self::State<'a>,
        private_state: Self::PrivateState<'a>,
        _provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<(Self::State<'a>, Self::PrivateState<'a>)> {
        let version = match private_state {
            Value::Value(version) => version.to_string(),
            Value::Null => String::new(),
            // Resource has been imported, but not yet updated
            Value::Unknown => return Some((state, private_state)),
        };

        let mut state_env = prepare_envs(&[(&state.inputs, "INPUT_"), (&state.state, "STATE_")]);
        state_env.push((Cow::from("ID"), Cow::from(state.id.as_str())));
        state_env.push((Cow::from("VERSION"), Cow::from(version)));

        let mut state = state.clone();
        state.normalize(diags);

        // Mark all values unknown to force their read
        state.state = Value::Value(
            state
                .read
                .iter()
                .flatten()
                .map(|(name, _)| (name.clone(), Value::Unknown))
                .collect(),
        );

        state.read(diags, &self.connect, &state_env, true).await;

        Some((state, private_state))
    }

    async fn plan_create<'a>(
        &self,
        diags: &mut Diagnostics,
        proposed_state: Self::State<'a>,
        _config_state: Self::State<'a>,
        _provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<(Self::State<'a>, Self::PrivateState<'a>)> {
        let mut state = proposed_state.clone();
        state.id = ValueString::Unknown;
        state.state = Value::Unknown;
        state.normalize(diags);

        Some((state, Default::default()))
    }
    async fn plan_update<'a>(
        &self,
        diags: &mut Diagnostics,
        prior_state: Self::State<'a>,
        proposed_state: Self::State<'a>,
        _config_state: Self::State<'a>,
        prior_private_state: Self::PrivateState<'a>,
        provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<(
        Self::State<'a>,
        Self::PrivateState<'a>,
        Vec<tf_provider::AttributePath>,
    )> {
        let value_map_default = Default::default();
        // Resource has been imported, but not yet updated.
        // The state is read from the config, before planning the update.
        let prior_state = if prior_private_state.is_unknown() {
            self.read(
                diags,
                ResourceState {
                    // Copy the inputs from the proposed state while removing the unknowns.
                    // An unknown input must trigger the related update
                    inputs: Value::Value(
                        proposed_state
                            .inputs
                            .as_ref()
                            .unwrap_or(&value_map_default)
                            .iter()
                            .filter_map(|(key, value)| {
                                if value.is_unknown() {
                                    None
                                } else {
                                    Some((key.clone(), value.clone()))
                                }
                            })
                            .collect(),
                    ),
                    ..proposed_state.clone()
                },
                Value::Value(0),
                provider_meta_state,
            )
            .await?
            .0
        } else {
            prior_state
        };

        let mut state = proposed_state.clone();
        state.normalize(diags);

        let previous_state = prior_state.state.as_ref().unwrap_or(&value_map_default);
        let previous_reads_default = Default::default();
        let previous_reads = prior_state.read.as_ref().unwrap_or(&previous_reads_default);

        match &state.read {
            Value::Value(reads) => {
                // Mark all values unknown to force their read
                state.state = Value::Value(
                    reads
                        .iter()
                        .map(|(name, read)| {
                            (
                                name.clone(),
                                match (previous_reads.get(name), previous_state.get(name)) {
                                    (_, None) => Value::Unknown,
                                    (None, Some(val)) => val.clone(),
                                    (Some(previous_read), Some(val)) => {
                                        if previous_read == read {
                                            val.clone()
                                        } else {
                                            Value::Unknown
                                        }
                                    }
                                },
                            )
                        })
                        .collect(),
                );
            }
            Value::Null => {
                state.read = Value::Value(Default::default());
                state.state = Value::Value(Default::default());
            }
            Value::Unknown => {
                state.state = Value::Unknown;
            }
        }

        let modified = find_modified(&prior_state.inputs, &proposed_state.inputs);
        let mut trigger_replace = Default::default();

        if let Some((update, _)) = find_update(&mut state.update, &modified) {
            if !modified.is_empty() || update.triggers == Value::Value(Default::default()) {
                update.update_triggered = Value::Unknown;
                if let Value::Value(outputs) = &mut state.state {
                    let reloads_default = Default::default();
                    let reloads = update.reloads.as_ref().unwrap_or(&reloads_default);
                    for name in reloads {
                        if let Some(value) = outputs.get_mut(name.as_str()) {
                            *value = Value::Unknown;
                        }
                    }
                }
            }
        } else if !modified.is_empty() {
            trigger_replace = modified
                .into_iter()
                .map(|name| AttributePath::new("inputs").key(name.unwrap_or_default().into_owned()))
                .collect();
        }

        Some((state, prior_private_state, trigger_replace))
    }

    async fn plan_destroy<'a>(
        &self,
        diags: &mut Diagnostics,
        _prior_state: Self::State<'a>,
        prior_private_state: Self::PrivateState<'a>,
        _provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<Self::PrivateState<'a>> {
        if prior_private_state.is_unknown() {
            diags.root_warning(
                "Destroy ignored on newly imported resource",
                "The resource has just been imported and need to be applied once in order to know how it should be destroyed.\nAs it has not been applied since import, it will be removed from state without calling the destroy command."
            );
        }
        Some(prior_private_state)
    }

    async fn create<'a>(
        &self,
        diags: &mut Diagnostics,
        planned_state: Self::State<'a>,
        _config_state: Self::State<'a>,
        mut private_state: Self::PrivateState<'a>,
        _provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<(Self::State<'a>, Self::PrivateState<'a>)> {
        let mut state = planned_state.clone();
        state.normalize(diags);

        let version = private_state.unwrap_or_default() + 1;
        private_state = Value::from(version);

        let id = state.extract_id();

        let connection_default = Default::default();
        let connection = planned_state
            .connect
            .as_ref()
            .unwrap_or(&connection_default);

        let mut state_env = prepare_envs(&[(&planned_state.inputs, "INPUT_")]);
        state_env.push((Cow::from("ID"), Cow::from(id.as_ref())));
        state_env.push((Cow::from("VERSION"), Cow::from(version.to_string())));

        let create_cmd = state.create.cmd();
        let create_dir = state.create.dir();
        if !create_cmd.is_empty() {
            let attr_path = AttributePath::new("create").index(0).attribute("cmd");
            match self
                .connect
                .execute(
                    connection,
                    create_cmd,
                    create_dir,
                    with_env(&state_env, state.create.env()),
                )
                .await
            {
                Ok(res) => {
                    if !res.stdout.is_empty() {
                        diags.warning(
                            "`create` stdout was not empty",
                            res.stdout,
                            attr_path.clone(),
                        );
                    }
                    if res.status == 0 {
                        if !res.stderr.is_empty() {
                            diags.warning(
                                "`create` succeeded but stderr was not empty",
                                res.stderr,
                                attr_path,
                            );
                        }
                    } else {
                        diags.error(
                            format!("`create` failed with status code: {}", res.status),
                            res.stderr,
                            attr_path,
                        );
                    }
                }
                Err(err) => {
                    diags.error("Failed to create resource", err.to_string(), attr_path);
                }
            }
        }

        if !diags.errors.is_empty() {
            return None;
        }

        state.read(diags, &self.connect, &state_env, false).await;

        state.id = Value::Value(id);

        Some((state, private_state))
    }
    async fn update<'a>(
        &self,
        diags: &mut Diagnostics,
        prior_state: Self::State<'a>,
        planned_state: Self::State<'a>,
        _config_state: Self::State<'a>,
        mut private_state: Self::PrivateState<'a>,
        _provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<(Self::State<'a>, Self::PrivateState<'a>)> {
        let connection_default = Default::default();
        let connection = planned_state
            .connect
            .as_ref()
            .unwrap_or(&connection_default);

        let version = private_state.unwrap_or_default() + 1;
        private_state = Value::from(version);

        let mut state = planned_state.clone();
        state.normalize(diags);
        let id = state.extract_id();

        let mut state_env = prepare_envs(&[
            (&planned_state.inputs, "INPUT_"),
            (&prior_state.inputs, "PREVIOUS_"),
            (&prior_state.state, "STATE_"),
        ]);
        state_env.push((Cow::from("ID"), Cow::from(id.as_ref())));
        state_env.push((Cow::from("VERSION"), Cow::from(version.to_string())));

        let mut updates_default = Default::default();
        for (i, update) in state
            .update
            .as_mut()
            .unwrap_or(&mut updates_default)
            .iter_mut()
            .enumerate()
        {
            let Value::Value(
                update @ StateUpdate {
                    update_triggered: Value::Unknown,
                    ..
                },
            ) = update
            else {
                continue;
            };

            let attr_path = AttributePath::new("update")
                .index(i as i64)
                .attribute("cmd");
            update.update_triggered = Value::Null;
            let update_cmd = update.cmd();
            let update_dir = update.dir();
            if !update_cmd.is_empty() {
                match self
                    .connect
                    .execute(
                        connection,
                        update_cmd,
                        update_dir,
                        with_env(&state_env, update.env()),
                    )
                    .await
                {
                    Ok(res) => {
                        if !res.stdout.is_empty() {
                            diags.warning(
                                "`update` stdout was not empty",
                                res.stdout,
                                attr_path.clone(),
                            );
                        }
                        if res.status == 0 {
                            if !res.stderr.is_empty() {
                                diags.warning(
                                    "`update` succeeded but stderr was not empty",
                                    res.stderr,
                                    attr_path,
                                );
                            }
                        } else {
                            diags.error(
                                format!("`update` failed with status code: {}", res.status),
                                res.stderr,
                                attr_path,
                            );
                        }
                    }
                    Err(err) => {
                        diags.error("Failed to update resource", err.to_string(), attr_path);
                    }
                }
            } else {
                diags.error_short("`update` cmd should not be null or empty", attr_path);
                return None;
            }
        }

        state.read(diags, &self.connect, &state_env, false).await;

        state.id = Value::Value(id);

        Some((state, private_state))
    }
    async fn destroy<'a>(
        &self,
        diags: &mut Diagnostics,
        state: Self::State<'a>,
        planned_private_state: Self::PrivateState<'a>,
        _provider_meta_state: Self::ProviderMetaState<'a>,
    ) -> Option<()> {
        let connection_default = Default::default();
        let connection = state.connect.as_ref().unwrap_or(&connection_default);

        let mut state_env = prepare_envs(&[(&state.inputs, "INPUT_"), (&state.state, "STATE_")]);
        state_env.push((Cow::from("ID"), Cow::from(state.id.as_str())));
        state_env.push((
            Cow::from("Version"),
            Cow::from(planned_private_state.unwrap_or(0).to_string()),
        ));

        let destroy_cmd = state.destroy.cmd();
        let destroy_dir = state.destroy.dir();
        if !destroy_cmd.is_empty() {
            let attr_path = AttributePath::new("destroy").index(0).attribute("cmd");
            match self
                .connect
                .execute(
                    connection,
                    destroy_cmd,
                    destroy_dir,
                    with_env(&state_env, state.destroy.env()),
                )
                .await
            {
                Ok(res) => {
                    if !res.stdout.is_empty() {
                        diags.warning(
                            "`destroy` stdout was not empty",
                            res.stdout,
                            attr_path.clone(),
                        );
                    }
                    if res.status == 0 {
                        if !res.stderr.is_empty() {
                            diags.warning(
                                "`destroy` succeeded but stderr was not empty",
                                res.stderr,
                                attr_path,
                            );
                        }
                    } else {
                        diags.error(
                            format!("`destroy` failed with status code: {}", res.status),
                            res.stderr,
                            attr_path,
                        );
                    }
                }
                Err(err) => {
                    diags.error("Failed to destroy resource", err.to_string(), attr_path);
                }
            }
        }
        Some(())
    }
    async fn import<'a>(
        &self,
        diags: &mut Diagnostics,
        id: String,
    ) -> Option<(Self::State<'a>, Self::PrivateState<'a>)> {
        let mut state = BTreeMap::new();
        for var in id.split(',') {
            if var.is_empty() {
                continue;
            }

            let (key, value) = var.split_once('=').unwrap_or((var, ""));
            state.insert(
                Cow::Owned(key.to_owned()),
                Value::Value(Cow::Owned(value.to_owned())),
            );
        }

        let mut state = Self::State {
            id: Value::Null,
            inputs: Value::Value(Default::default()),
            state: Value::Value(state),
            read: Value::Value(Default::default()),
            create: Value::Null,
            destroy: Value::Null,
            update: Value::Value(Default::default()),
            connect: Value::Null,
            command_concurrency: Value::Null,
        };
        state.id = Value::Value(state.extract_id());
        state.normalize(diags);
        Some((state, Value::Unknown))
    }
}

fn find_modified<'a>(
    state: &'a ValueMap<'a, ValueString<'a>>,
    plan: &'a ValueMap<'a, ValueString<'a>>,
) -> BTreeSet<ValueString<'a>> {
    match (state, plan) {
        (Value::Value(state), Value::Value(plan)) => {
            let mut modified = BTreeSet::new();

            for (k, x) in state {
                if let Some(y) = plan.get(k) {
                    if x != y {
                        modified.insert(Value::Value(Cow::from(k.as_ref())));
                    }
                } else {
                    modified.insert(Value::Value(Cow::from(k.as_ref())));
                }
            }
            for k in plan.keys() {
                if !state.contains_key(k) {
                    modified.insert(Value::Value(Cow::from(k.as_ref())));
                }
            }

            modified
        }
        (_, Value::Value(plan)) => plan
            .keys()
            .map(|k| Value::Value(Cow::from(k.as_ref())))
            .collect(),
        (Value::Value(state), _) => state
            .keys()
            .map(|k| Value::Value(Cow::from(k.as_ref())))
            .collect(),
        _ => Default::default(),
    }
}

fn find_update<'a, 'b, 'c>(
    updates: &'b mut ValueList<Value<StateUpdate<'a>>>,
    modified: &'c BTreeSet<ValueString<'c>>,
) -> Option<(&'b mut StateUpdate<'a>, AttributePath)> {
    let empty_set = Default::default();
    let updates = updates.as_mut_option()?;

    let mut found: Option<(&'b mut StateUpdate<'a>, usize)> = None;
    for (i, update) in updates.iter_mut().flatten().enumerate() {
        match &update.triggers {
            Value::Value(triggers) => {
                if triggers.is_superset(modified) {
                    if let Some(found) = &mut found {
                        let previous_triggers = found.0.triggers.as_ref().unwrap_or(&empty_set);
                        if previous_triggers.len() > triggers.len() {
                            *found = (update, i);
                        }
                    } else {
                        found = Some((update, i));
                    }
                }
            }
            _ => {
                if found.is_none() {
                    found = Some((update, i));
                }
            }
        }
    }
    found.map(|(update, i)| (update, AttributePath::new("update").index(i as i64)))
}

impl<'a, T: Connection> ResourceState<'a, T> {
    fn extract_id(&mut self) -> Cow<'a, str> {
        if let Value::Value(id) = std::mem::take(&mut self.id) {
            id
        } else {
            thread_rng()
                .sample_iter(&Alphanumeric)
                .take(30)
                .map(char::from)
                .collect()
        }
    }
}

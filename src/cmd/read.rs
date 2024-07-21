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

use futures::{stream, StreamExt};
use tf_provider::value::{Value, ValueMap, ValueNumber, ValueString};
use tf_provider::{AttributePath, Diagnostics};

use crate::{
    connection::Connection,
    utils::{WithEnv, WithRead},
};

use super::{
    state::{DataSourceState, ResourceState},
    with_env,
};

impl<'a, T: Connection> ResourceState<'a, T> {
    pub async fn read<'b>(
        &mut self,
        diags: &mut Diagnostics,
        connect: &T,
        env: &[(Cow<'b, str>, Cow<'b, str>)],
        faillibe: bool,
    ) -> Option<()> {
        read_all(
            diags,
            connect,
            &self.connect,
            &self.read,
            &mut self.state,
            env,
            faillibe,
            self.command_concurrency,
        )
        .await
    }
}

impl<'a, T: Connection> DataSourceState<'a, T> {
    pub async fn read<'b>(
        &mut self,
        diags: &mut Diagnostics,
        connect: &T,
        env: &[(Cow<'b, str>, Cow<'b, str>)],
    ) -> Option<()> {
        read_all(
            diags,
            connect,
            &self.connect,
            &self.read,
            &mut self.outputs,
            env,
            false,
            self.command_concurrency,
        )
        .await
    }
}

#[allow(clippy::too_many_arguments)]
async fn read_all<'a, 'b, C, R>(
    diags: &mut Diagnostics,
    connect: &C,
    connect_config: &Value<C::Config<'a>>,
    reads: &ValueMap<'a, Value<R>>,
    outputs: &mut ValueMap<'a, ValueString<'a>>,
    env: &[(Cow<'b, str>, Cow<'b, str>)],
    faillibe: bool,
    concurrency: ValueNumber,
) -> Option<()>
where
    C: Connection,
    R: WithRead + WithEnv<Env = ValueMap<'a, ValueString<'a>>>,
{
    let outputs = outputs.as_mut_option()?;

    let connection_default = Default::default();
    let connect_config = connect_config.as_ref().unwrap_or(&connection_default);

    let reads_default = Default::default();
    let reads = reads.as_ref().unwrap_or(&reads_default);

    let concurrency = concurrency.unwrap_or(4) as usize;

    let mut read_tasks = Vec::new();

    for (name, value) in outputs.iter_mut() {
        if !value.is_unknown() {
            continue;
        }
        if let Some(Value::Value(read)) = reads.get(name) {
            let cmd = read.cmd();
            let dir = read.dir();

            read_tasks.push(async move {
                let result = connect
                    .execute(connect_config, cmd, dir, with_env(env, read.env()))
                    .await;
                (
                    name,
                    value,
                    faillibe || read.faillible(),
                    read.strip_trailing_newline(),
                    result,
                )
            });
        } else {
            diags.error(
                    "Unknown output has no `read` block associated",
                    format!("The output `state.{name}` is unknown, and there is no known `read[\"{name}\"]` block to give it a value."),
                    AttributePath::new("state").key(name.to_string())
                );
        }
    }

    for (name, value, faillible, strip_trailing_newline, result) in
        stream::iter(read_tasks.into_iter())
            .buffer_unordered(concurrency)
            .collect::<Vec<_>>()
            .await
    {
        let attr_path = AttributePath::new("read")
            .key(name.to_string())
            .attribute("cmd");
        *value = Value::Null;
        let report: fn(&mut Diagnostics, String, String, AttributePath) = if faillible {
            Diagnostics::warning
        } else {
            Diagnostics::error
        };
        match result {
            Ok(res) => {
                if res.status == 0 {
                    if !res.stderr.is_empty() {
                        diags.warning(
                            "`read` succeeded but stderr was not empty",
                            res.stderr,
                            attr_path,
                        );
                    }
                    let mut stdout: Cow<'_, _> = res.stdout.into();

                    if strip_trailing_newline && stdout.as_bytes()[stdout.len() - 1] == b'\n' {
                        stdout = match stdout {
                            Cow::Borrowed(s) => Cow::Borrowed(&s[0..s.len() - 1]),
                            Cow::Owned(mut s) => {
                                s.pop();
                                Cow::Owned(s)
                            }
                        }
                    }

                    *value = Value::Value(stdout);
                } else {
                    report(
                        diags,
                        format!("`read` failed with status code: {}", res.status),
                        res.stderr,
                        attr_path,
                    );
                }
            }
            Err(err) => {
                report(
                    diags,
                    "Failed to read resource state".to_string(),
                    err.to_string(),
                    attr_path,
                );
            }
        }
    }

    Some(())
}

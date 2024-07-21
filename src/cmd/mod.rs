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

use tf_provider::value::{ValueMap, ValueString};

mod data_source;
mod normalize;
mod read;
mod resource;
mod state;
mod validate;

pub use data_source::GenericCmdDataSource;
pub use resource::GenericCmdResource;

fn prepare_envs<'a>(
    envs: &[(&'a ValueMap<'a, ValueString<'a>>, &'a str)],
) -> Vec<(Cow<'a, str>, Cow<'a, str>)> {
    envs.iter()
        .flat_map(|(env, prefix)| {
            env.iter().flatten().filter_map(|(k, v)| {
                Some((
                    Cow::Owned(format!("{}{}", *prefix, k)),
                    Cow::Borrowed(v.as_deref_option()?),
                ))
            })
        })
        .collect()
}

fn with_env<'a>(
    base_env: &'a [(Cow<'a, str>, Cow<'a, str>)],
    extra_env: &'a ValueMap<'a, ValueString<'a>>,
) -> impl Iterator<Item = (&'a Cow<'a, str>, &'a Cow<'a, str>)> {
    base_env.iter().map(|(k, v)| (k, v)).chain(
        extra_env
            .iter()
            .flatten()
            .filter_map(|(k, v)| Some((k, v.as_ref_option()?))),
    )
}

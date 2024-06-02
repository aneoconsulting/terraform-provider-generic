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

use tf_provider::{value::Value, Diagnostics};

use crate::{connection::Connection, utils::WithNormalize};

use super::state::ResourceState;

impl<'a, T: Connection> WithNormalize for ResourceState<'a, T> {
    fn normalize(&mut self, _diags: &mut Diagnostics) {
        if self.id.is_null() {
            self.id = Value::Unknown;
        }
        if self.inputs.is_null() {
            self.inputs = Value::Value(Default::default());
        }
        if self.state.is_unknown() {
            self.state = Value::Value(
                self.read
                    .iter()
                    .flatten()
                    .map(|(name, _)| (name.clone(), Value::Unknown))
                    .collect(),
            );
        }
    }
}

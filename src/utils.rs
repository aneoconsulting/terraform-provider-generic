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

use std::cell::RefCell;

use async_trait::async_trait;

use tf_provider::{AttributePath, Diagnostics, Schema, Value};

pub(crate) trait WithSchema {
    fn schema() -> Schema;
}

#[async_trait]
pub(crate) trait WithValidate {
    async fn validate(&self, diags: &mut Diagnostics, attr_path: AttributePath);
}

pub(crate) trait WithNormalize {
    fn normalize(&mut self, diags: &mut Diagnostics);
}

pub(crate) trait WithCmd {
    fn cmd(&self) -> &str;
    fn dir(&self) -> &str;
}

impl<T: WithCmd> WithCmd for Value<T> {
    fn cmd(&self) -> &str {
        self.as_ref().map_or("", WithCmd::cmd)
    }
    fn dir(&self) -> &str {
        self.as_ref().map_or("", WithCmd::dir)
    }
}

pub(crate) trait WithRead: WithCmd {
    fn strip_trailing_newline(&self) -> bool;
    fn faillible(&self) -> bool;
}

impl<T: WithRead> WithRead for Value<T> {
    fn strip_trailing_newline(&self) -> bool {
        self.as_ref().map_or(true, WithRead::strip_trailing_newline)
    }
    fn faillible(&self) -> bool {
        self.as_ref().map_or(true, WithRead::faillible)
    }
}

pub(crate) trait WithEnv {
    type Env;
    fn env(&self) -> &Self::Env;
}

impl<T, E> WithEnv for Value<T>
where
    T: WithEnv<Env = Value<E>>,
{
    type Env = T::Env;
    fn env(&self) -> &Self::Env {
        self.as_ref().map_or(&Value::Null, WithEnv::env)
    }
}

pub struct DisplayJoiner<'a, T, I>
where
    T: Iterator<Item = I>,
    I: std::fmt::Display,
{
    iter: RefCell<T>,
    sep: &'a str,
}

pub trait DisplayJoinable {
    type Joiner<'a>;
    fn join_with(self, sep: &str) -> Self::Joiner<'_>;
}

impl<T, I> DisplayJoinable for T
where
    T: Iterator<Item = I>,
    I: std::fmt::Display,
{
    type Joiner<'a> = DisplayJoiner<'a, T, I>;

    fn join_with(self, sep: &str) -> Self::Joiner<'_> {
        DisplayJoiner {
            iter: RefCell::new(self),
            sep,
        }
    }
}

impl<'a, T, I> std::fmt::Display for DisplayJoiner<'a, T, I>
where
    T: Iterator<Item = I>,
    I: std::fmt::Display,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut sep = "";
        let mut iter = self.iter.try_borrow_mut().or(Err(std::fmt::Error))?;
        for elt in iter.by_ref() {
            f.write_str(sep)?;
            f.write_fmt(format_args!("{elt}"))?;
            sep = self.sep;
        }
        Ok(())
    }
}

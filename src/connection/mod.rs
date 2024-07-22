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

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tf_provider::{schema::Attribute, AttributePath, Diagnostics};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::utils::AsyncDrop;

pub mod local;
pub mod ssh;

#[derive(Debug, PartialEq, Eq)]
pub struct ExecutionResult {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait]
pub trait Connection: Send + Sync + 'static + Default {
    const NAME: &'static str;
    type Config<'a>: Send
        + Sync
        + Clone
        + std::fmt::Debug
        + Default
        + Serialize
        + for<'de> Deserialize<'de>;
    type Reader: AsyncRead + Send + AsyncDrop;
    type Writer: AsyncWrite + Send + AsyncDrop;

    /// execute a command over the connection
    async fn execute<'a, 'b, I, K, V>(
        &self,
        config: &Self::Config<'a>,
        cmd: &str,
        dir: &str,
        env: I,
    ) -> Result<ExecutionResult>
    where
        'a: 'b,
        I: IntoIterator<Item = (&'b K, &'b V)> + Send + Sync + 'b,
        I::IntoIter: Send + Sync + 'b,
        K: AsRef<str> + Send + Sync + 'b,
        V: AsRef<str> + Send + Sync + 'b;

    /// Return a reader to read a remote file
    async fn read<'a>(&self, config: &Self::Config<'a>, path: &str) -> Result<Self::Reader>;

    /// Return a writer to write a remote file
    async fn write<'a>(
        &self,
        config: &Self::Config<'a>,
        path: &str,
        mode: u32,
        overwrite: bool,
    ) -> Result<Self::Writer>;

    /// Delete a file
    async fn delete<'a>(&self, config: &Self::Config<'a>, path: &str) -> Result<()>;

    /// Validate the state is valid
    async fn validate<'a>(
        &self,
        diags: &mut Diagnostics,
        attr_path: AttributePath,
        config: &Self::Config<'a>,
    ) -> Option<()>;

    /// Get the schema for the connection block
    fn schema() -> HashMap<String, Attribute>;
}

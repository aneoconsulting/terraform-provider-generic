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

use std::{collections::HashMap, pin::Pin, sync::Arc};

use crate::{
    connection::{Connection, ExecutionResult},
    utils::AsyncDrop,
};
use anyhow::Result;
use async_trait::async_trait;
use futures::Future;
use rusftp::{
    client::{Error, File, SftpClient},
    message::{Attrs, PFlags, Permisions, Status, StatusCode},
};
use serde::{Deserialize, Serialize};
use tf_provider::schema::{Attribute, AttributeConstraint, AttributeType, Description};
use tf_provider::value::{Value, ValueString};
use tf_provider::{map, AttributePath, Diagnostics};
use tokio::sync::Mutex;

mod client;

use client::Client;

#[derive(Default, Clone)]
pub struct ConnectionSsh {
    clients: Arc<Mutex<HashMap<ConnectionSshConfig<'static>, Arc<Client>>>>,
}

impl ConnectionSsh {
    fn get_client<'a>(
        &'a self,
        config: &ConnectionSshConfig<'a>,
    ) -> impl Future<Output = Result<Arc<Client>>> + Send + 'a {
        let config = config.clone();
        async move {
            let mut clients = self.clients.lock().await;
            let client = match clients.entry(config.extend()) {
                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::hash_map::Entry::Vacant(entry) => {
                    let client = Client::connect(entry.key()).await?;
                    entry.insert(Arc::new(client))
                }
            };

            Ok(client.clone())
        }
    }
}

impl Drop for ConnectionSsh {
    fn drop(&mut self) {
        let clients = Pin::new(futures::executor::block_on(self.clients.lock()));

        for (_, client) in clients.iter() {
            _ = futures::executor::block_on(client.disconnect());
        }
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize, Hash, Default, Clone)]
pub struct ConnectionSshConfig<'a> {
    pub host: ValueString<'a>,
    pub port: Value<u16>,
    pub user: ValueString<'a>,
    pub password: ValueString<'a>,
    pub key: ValueString<'a>,
    pub keyfile: ValueString<'a>,
}

impl<'a> ConnectionSshConfig<'a> {
    fn extend<'b>(self) -> ConnectionSshConfig<'b> {
        ConnectionSshConfig {
            host: self.host.extend(),
            port: self.port,
            user: self.user.extend(),
            password: self.password.extend(),
            key: self.key.extend(),
            keyfile: self.keyfile.extend(),
        }
    }
}

#[async_trait]
impl Connection for ConnectionSsh {
    const NAME: &'static str = "ssh";
    type Config<'a> = ConnectionSshConfig<'a>;
    type Reader = File;
    type Writer = File;

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
        V: AsRef<str> + Send + Sync + 'b,
    {
        let client = self.get_client(config).await?;
        let result = client.execute(cmd, dir, env).await?;
        Ok(result)
    }

    /// Return a reader to read a remote file
    async fn read<'a>(&self, config: &Self::Config<'a>, path: &str) -> Result<Self::Reader> {
        let ssh = self.get_client(config).await?;
        let sftp = SftpClient::new(&ssh.handle).await?;

        Ok(sftp.open_with_flags(path, PFlags::READ).await?)
    }

    /// Return a writer to write a remote file
    async fn write<'a>(
        &self,
        config: &Self::Config<'a>,
        path: &str,
        mode: u32,
        overwrite: bool,
    ) -> Result<Self::Writer> {
        let ssh = self.get_client(config).await?;
        let sftp = SftpClient::new(&ssh.handle).await?;

        let mut flags = PFlags::WRITE | PFlags::CREATE;
        if overwrite {
            flags |= PFlags::TRUNCATE;
        } else {
            // Check if file exist in case the EXCLUDE flag is not taken into account
            match sftp.lstat(path).await {
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "File already exists",
                    )
                    .into())
                }
                Err(Error::Sftp(Status {
                    code: StatusCode::NoSuchFile,
                    ..
                })) => (),
                Err(err) => {
                    return Err(err.into());
                }
            }
            flags |= PFlags::EXCLUDE;
        }

        let file = sftp
            .open_with_flags_attrs(
                path,
                flags,
                Attrs {
                    perms: Some(Permisions::from_bits_retain(mode)),
                    ..Default::default()
                },
            )
            .await?;

        Ok(file)
    }

    /// Delete a file
    async fn delete<'a>(&self, config: &Self::Config<'a>, path: &str) -> Result<()> {
        let client = self.get_client(config).await?;
        let client = SftpClient::new(&client.handle).await?;

        Ok(client.remove(path).await?)
    }

    /// Validate the state is valid
    async fn validate<'a>(
        &self,
        diags: &mut Diagnostics,
        attr_path: AttributePath,
        config: &Self::Config<'a>,
    ) -> Option<()> {
        match &config.host {
            Value::Value(host) => {
                if host.is_empty() {
                    diags.error_short("`hostname` cannot be empty", attr_path.attribute("host"));
                    return None;
                }
            }
            Value::Null => {
                diags.error_short("`hostname` cannot be null", attr_path.attribute("host"));
                return None;
            }
            Value::Unknown => (),
        }
        Some(())
    }

    fn schema() -> HashMap<String, Attribute> {
        map! {
            "host" => Attribute {
                attr_type: AttributeType::String,
                description: Description::plain("Hostname to connect to"),
                constraint: AttributeConstraint::Required,
                ..Default::default()
            },
            "port" => Attribute {
                attr_type: AttributeType::Number,
                description: Description::plain("Port to connect to"),
                constraint: AttributeConstraint::Optional,
                ..Default::default()
            },
            "user" => Attribute {
                attr_type: AttributeType::String,
                description: Description::plain("User to connect with"),
                constraint: AttributeConstraint::Optional,
                ..Default::default()
            },
            "password" => Attribute {
                attr_type: AttributeType::String,
                description: Description::plain("Password or passphrase"),
                constraint: AttributeConstraint::Optional,
                ..Default::default()
            },
            "key" => Attribute {
                attr_type: AttributeType::String,
                description: Description::plain("Key"),
                constraint: AttributeConstraint::Optional,
                ..Default::default()
            },
            "keyfile" => Attribute {
                attr_type: AttributeType::String,
                description: Description::plain("Filename of the key"),
                constraint: AttributeConstraint::Optional,
                ..Default::default()
            },
        }
    }
}

impl std::fmt::Debug for ConnectionSsh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionSsh") /*.field("clients", &self.clients)*/
            .finish()
    }
}

#[async_trait]
impl AsyncDrop for File {
    async fn async_drop(&mut self) {
        _ = self.close().await;
    }
}

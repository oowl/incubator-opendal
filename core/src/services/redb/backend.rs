// Licensed to the Apache Software Foundation (ASF) under one
// or more contributor license agreements.  See the NOTICE file
// distributed with this work for additional information
// regarding copyright ownership.  The ASF licenses this file
// to you under the Apache License, Version 2.0 (the
// "License"); you may not use this file except in compliance
// with the License.  You may obtain a copy of the License at
//
//   http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing,
// software distributed under the License is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied.  See the License for the
// specific language governing permissions and limitations
// under the License.

use std::collections::HashMap;
use std::fmt::Debug;
use std::fmt::Formatter;
use std::sync::Arc;

use async_trait::async_trait;
use redb::ReadableTable;

use crate::raw::adapters::kv;
use crate::Builder;
use crate::Error;
use crate::ErrorKind;
use crate::Scheme;
use crate::*;

/// Redb service support.
#[doc = include_str!("docs.md")]
#[derive(Default)]
pub struct RedbBuilder {
    /// That path to the redb data directory.
    datadir: Option<String>,
    root: Option<String>,
    table: Option<String>,
}

impl RedbBuilder {
    /// Set the path to the redb data directory. Will create if not exists.
    pub fn datadir(&mut self, path: &str) -> &mut Self {
        self.datadir = Some(path.into());
        self
    }

    /// Set the table name for Redb.
    pub fn table(&mut self, table: &str) -> &mut Self {
        self.table = Some(table.into());
        self
    }

    /// Set the root for Redb.
    pub fn root(&mut self, path: &str) -> &mut Self {
        self.root = Some(path.into());
        self
    }
}

impl Builder for RedbBuilder {
    const SCHEME: Scheme = Scheme::Redb;
    type Accessor = RedbBackend;

    fn from_map(map: HashMap<String, String>) -> Self {
        let mut builder = RedbBuilder::default();

        map.get("datadir").map(|v| builder.datadir(v));
        map.get("root").map(|v| builder.root(v));

        builder
    }

    fn build(&mut self) -> Result<Self::Accessor> {
        let datadir_path = self.datadir.take().ok_or_else(|| {
            Error::new(ErrorKind::ConfigInvalid, "datadir is required but not set")
                .with_context("service", Scheme::Redb)
        })?;

        let table_name = self.table.take().ok_or_else(|| {
            Error::new(ErrorKind::ConfigInvalid, "table is required but not set")
                .with_context("service", Scheme::Redb)
        })?;

        let db = redb::Database::create(&datadir_path).map_err(|e| {
            Error::new(ErrorKind::ConfigInvalid, "open db")
                .with_context("service", Scheme::Redb)
                .with_context("datadir", datadir_path.clone())
                .set_source(e)
        })?;

        let db = Arc::new(db);

        Ok(RedbBackend::new(Adapter {
            datadir: datadir_path,
            table: table_name,
            db,
        })
        .with_root(self.root.as_deref().unwrap_or_default()))
    }
}

/// Backend for Redb services.
pub type RedbBackend = kv::Backend<Adapter>;

#[derive(Clone)]
pub struct Adapter {
    datadir: String,
    table: String,
    db: Arc<redb::Database>,
}

impl Debug for Adapter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let mut ds = f.debug_struct("Adapter");
        ds.field("path", &self.datadir);
        ds.finish()
    }
}

#[async_trait]
impl kv::Adapter for Adapter {
    fn metadata(&self) -> kv::Metadata {
        kv::Metadata::new(
            Scheme::Redb,
            &self.datadir,
            Capability {
                read: true,
                write: true,
                blocking: true,
                ..Default::default()
            },
        )
    }

    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>> {
        self.blocking_get(path)
    }

    fn blocking_get(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let read_txn = match self.db.begin_read() {
            Ok(txn) => txn,
            Err(e) => {
                return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
            }
        };
        
        let table_define: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new(&self.table);

        let table = match read_txn.open_table(table_define) {
            Ok(table) => table,
            Err(e) => {
                return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
            }
        };

        let result = match table.get(path) {
            Ok(Some(v)) => Ok(Some(v.value().to_vec())),
            Ok(None) => Ok(None),
            Err(e) => Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e)),
        };
        result
    }

    async fn set(&self, path: &str, value: &[u8]) -> Result<()> {
        self.blocking_set(path, value)
    }

    fn blocking_set(&self, path: &str, value: &[u8]) -> Result<()> {
        let write_txn = match self.db.begin_write() {
            Ok(txn) => txn,
            Err(e) => {
                return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
            }
        };

        let table_define: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new(&self.table);

        {
            let mut table = match write_txn.open_table(table_define) {
                Ok(table) => table,
                Err(e) => {
                    return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
                }
            };
    
            if let Err(e) = table.insert(path, value) {
                return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
            };
        }

        let result = match write_txn.commit() {
            Ok(()) => Ok(()),
            Err(e) => Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e)),
        };
        result
    }

    async fn delete(&self, path: &str) -> Result<()> {
        self.blocking_delete(path)
    }

    fn blocking_delete(&self, path: &str) -> Result<()> {
        let write_txn = match self.db.begin_write() {
            Ok(txn) => txn,
            Err(e) => {
                return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
            }
        };

        let table_define: redb::TableDefinition<&str, &[u8]> = redb::TableDefinition::new(&self.table);

        {
            let mut table = match write_txn.open_table(table_define) {
                Ok(table) => table,
                Err(e) => {
                    return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
                }
            };
        
            if let Err(e) = table.remove(path) {
                return Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e));
            };
        }

        let result = match write_txn.commit() {
            Ok(()) => Ok(()),
            Err(e) => Err(Error::new(ErrorKind::Unexpected, "error from redb").set_source(e)),
        };
        result
    }
}
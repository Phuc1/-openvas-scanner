// SPDX-FileCopyrightText: 2023 Greenbone AG
//
// SPDX-License-Identifier: GPL-2.0-or-later

use async_trait::async_trait;
use models::NotusResults;
use notus::{error::Error, loader::json::JSONAdvisoryLoader, notus::Notus};
use tokio::sync::RwLock;

#[async_trait]
pub trait NotusScanner {
    async fn scan(&self, os: &str, packages: &Vec<String>) -> Result<NotusResults, Error>;
}

#[derive(Debug)]
pub struct NotusWrapper {
    notus: RwLock<Notus<JSONAdvisoryLoader<String>>>,
}

impl NotusWrapper {
    pub fn new(notus: Notus<JSONAdvisoryLoader<String>>) -> Self {
        Self {
            notus: RwLock::new(notus),
        }
    }
}

#[async_trait]
impl NotusScanner for NotusWrapper {
    async fn scan(&self, os: &str, packages: &Vec<String>) -> Result<NotusResults, Error> {
        self.notus.write().await.scan(os, packages)
    }
}

// Copyright (C) 2023 Greenbone Networks GmbH
//
// SPDX-License-Identifier: GPL-2.0-or-later
mod error;

pub use error::Error;

use std::fs::File;

use nasl_interpreter::{AsBufReader, ContextType, Interpreter, Loader, NaslValue, Register};
use sink::{nvt::NVTField, Sink};

use crate::verify;

/// Updates runs nasl plugin with description true and uses given storage to store the descriptive
/// information
pub struct Update<S, L, V> {
    /// Is used to store data
    sink: S,
    /// Is used to load nasl plugins by a relative path
    loader: L,
    /// Initial data, usually set in new.
    initial: Vec<(String, ContextType)>,
    /// How often loader or storage should retry before giving up when a retryable error occurs.
    max_retry: usize,
    verifier: V,
    feed_version_set: bool,
}

impl From<verify::Error> for Error {
    fn from(value: verify::Error) -> Self {
        Error::VerifyError(value)
    }
}

impl<S, L, V> Update<S, L, V>
where
    S: Sync + Send + Sink,
    L: Sync + Send + Loader + AsBufReader<File>,
    V: Iterator<Item = Result<String, verify::Error>>,
{
    /// Creates an updater. This updater is implemented as a iterator.
    ///
    /// When calling the first .next() it will handle the corresponding plugin_feed_info.inc and
    /// set the feed version with it. Afterwards it will iterate through the given Verifier and
    /// execute the found .nasl file in description mode.
    /// This is usually done to update the feed.
    pub fn init(
        openvas_version: &str,
        max_retry: usize,
        loader: L,
        storage: S,
        verifier: V,
    ) -> impl Iterator<Item = Result<String, Error>> {
        let initial = vec![
            ("description".to_owned(), true.into()),
            ("OPENVAS_VERSION".to_owned(), openvas_version.into()),
        ];
        Self {
            initial,
            max_retry,
            loader,
            sink: storage,
            verifier,
            feed_version_set: false,
        }
    }

    /// plugin_feed_info must be handled differently.
    ///
    /// Usually a plugin_feed_info.inc is setup as a listing of keys.
    /// The feed_version is loaded from that inc file.
    /// Therefore we need to load the plugin_feed_info and extract the feed_version
    /// to put into the corresponding sink.
    fn plugin_feed_info(&self) -> Result<String, Error> {
        let feed_info_key = "plugin_feed_info.inc";
        let code = self.loader.load(feed_info_key)?;
        let mut register = Register::default();
        let mut interpreter = Interpreter::new("inc", &self.sink, &self.loader, &mut register);
        for stmt in nasl_syntax::parse(&code) {
            match stmt {
                Ok(stmt) => interpreter.retry_resolve(&stmt, self.max_retry)?,
                Err(e) => return Err(e.into()),
            };
        }

        let feed_version = register
            .named("PLUGIN_SET")
            .map(|x| x.to_string())
            .unwrap_or_else(|| "0".to_owned());
        self.sink.retry_dispatch(
            self.max_retry,
            feed_info_key,
            NVTField::Version(feed_version).into(),
        )?;
        Ok(feed_info_key.into())
    }

    /// Runs a single plugin in description mode.
    fn single<K>(&self, key: K) -> Result<i64, Error>
    where
        K: AsRef<str>,
    {
        let code = self.loader.load(key.as_ref())?;

        let mut register = Register::root_initial(&self.initial);
        let mut interpreter =
            Interpreter::new(key.as_ref(), &self.sink, &self.loader, &mut register);
        for stmt in nasl_syntax::parse(&code) {
            match interpreter.retry_resolve(&stmt?, self.max_retry) {
                Ok(NaslValue::Exit(i)) => {
                    self.sink.on_exit()?;
                    return Ok(i);
                }
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::MissingExit(key.as_ref().into()))
    }
}

impl<S, L, V> Iterator for Update<S, L, V>
where
    S: Sync + Send + Sink,
    L: Sync + Send + Loader + AsBufReader<File>,
    V: Iterator<Item = Result<String, verify::Error>>,
{
    type Item = Result<String, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.feed_version_set {
            let result = self.plugin_feed_info();
            self.feed_version_set = true;
            return Some(result);
        }
        let r = self.verifier.find(|x| {
            if let Ok(x) = x {
                x.ends_with(".nasl")
            } else {
                true
            }
        })?;
        match r {
            Ok(k) => self.single(&k).map(|_| k).into(),
            Err(e) => Some(Err(e.into())),
        }
    }
}

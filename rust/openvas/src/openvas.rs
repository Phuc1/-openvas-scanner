use async_trait::async_trait;
use models::{
    scanner::{
        Error as ScanError, ScanDeleter, ScanResultFetcher, ScanResults, ScanStarter, ScanStopper,
    },
    HostInfo, Phase, Scan, Status,
};
use redis_storage::{NameSpaceSelector, RedisCtx};
use std::{
    collections::HashMap,
    process::Child,
    str::FromStr,
    sync::{Arc, Mutex},
};

use crate::{
    cmd,
    error::OpenvasError,
    openvas_redis::{KbAccess, RedisHelper},
    pref_handler::PreferenceHandler,
    result_collector::ResultHelper,
};

#[derive(Debug)]
pub struct Scanner {
    running: Mutex<HashMap<String, (Child, u32)>>,
    sudo: bool,
    redis_socket: String,
}

impl From<OpenvasError> for ScanError {
    fn from(value: OpenvasError) -> Self {
        ScanError::Unexpected(value.to_string())
    }
}

impl Scanner {
    pub fn with_sudo_enabled() -> Self {
        Self {
            running: Default::default(),
            sudo: true,
            redis_socket: String::new(),
        }
    }

    pub fn with_sudo_disabled() -> Self {
        Self {
            running: Default::default(),
            sudo: false,
            redis_socket: String::new(),
        }
    }
    /// Removes a scan from init and add it to the list of running scans
    fn add_running(&self, id: String, dbid: u32) -> Result<bool, OpenvasError> {
        let openvas = cmd::start(&id, self.sudo, None).map_err(OpenvasError::CmdError)?;
        self.running.lock().unwrap().insert(id, (openvas, dbid));
        Ok(true)
    }

    /// Remove a scan from the list of running scans and returns the process to able to tidy up
    fn remove_running(&self, id: &str) -> Option<(Child, u32)> {
        self.running.lock().unwrap().remove(id)
    }

    fn create_redis_connector(&self, dbid: Option<u32>) -> RedisHelper<RedisCtx> {
        let namespace = match dbid {
            Some(id) => [NameSpaceSelector::Fix(id)],
            None => [NameSpaceSelector::Free],
        };

        let kbctx = Arc::new(Mutex::new(
            RedisCtx::open(&self.redis_socket, &namespace)
                .expect("Not possible to connect to Redis"),
        ));
        let nvtcache = Arc::new(Mutex::new(
            RedisCtx::open(&self.redis_socket, &[NameSpaceSelector::Key("nvticache")])
                .expect("Not possible to connect to Redis"),
        ));
        RedisHelper::<RedisCtx>::new(nvtcache, kbctx)
    }
}

impl Default for Scanner {
    fn default() -> Self {
        Self {
            running: Default::default(),
            sudo: cmd::check_sudo(),
            redis_socket: cmd::get_redis_socket(),
        }
    }
}
#[async_trait]
impl ScanStarter for Scanner {
    async fn start_scan(&self, scan: Scan) -> Result<(), ScanError> {
        // Prepare the connections to redis for communication with openvas.
        let mut redis_help = self.create_redis_connector(None);

        // Prepare preferences and store them in redis
        let mut pref_handler = PreferenceHandler::new(scan.clone(), &mut redis_help);
        match pref_handler.prepare_preferences_for_openvas().await {
            Ok(_) => (),
            Err(e) => {
                return Err(ScanError::Unexpected(e.to_string()));
            }
        }

        self.add_running(
            scan.scan_id,
            redis_help.kb_id().expect("Valid Redis context"),
        )?;

        return Ok(());
    }
}

/// Stops a scan
#[async_trait]
impl ScanStopper for Scanner {
    /// Stops a scan
    async fn stop_scan<I>(&self, id: I) -> Result<(), ScanError>
    where
        I: AsRef<str> + Send + 'static,
    {
        let scan_id = id.as_ref();

        let (mut scan, dbid) = match self.remove_running(scan_id) {
            Some(scan) => (scan.0, scan.1),
            None => return Err(OpenvasError::ScanNotFound(scan_id.to_string()).into()),
        };

        cmd::stop(scan_id, self.sudo)
            .map_err(OpenvasError::CmdError)?
            .wait()
            .map_err(OpenvasError::CmdError)?;

        scan.wait().map_err(OpenvasError::CmdError)?;

        // Release the task kb
        let mut redis_help = self.create_redis_connector(Some(dbid));
        redis_help
            .release()
            .map_err(|e| ScanError::Unexpected(e.to_string()))?;

        Ok(())
    }
}

/// Deletes a scan
#[async_trait]
impl ScanDeleter for Scanner {
    async fn delete_scan<I>(&self, id: I) -> Result<(), ScanError>
    where
        I: AsRef<str> + Send + 'static,
    {
        let scan_id = id.as_ref();

        let dbid = match self
            .running
            .lock()
            .map_err(|e| ScanError::Unexpected(e.to_string()))?
            .get(scan_id)
        {
            Some(scan) => scan.1,
            None => return Err(OpenvasError::ScanNotFound(scan_id.to_string()).into()),
        };

        let mut redis_help = self.create_redis_connector(Some(dbid));
        let mut ov_results = ResultHelper::init(&mut redis_help);
        ov_results
            .collect_scan_status(scan_id.to_string())
            .await
            .map_err(|e| ScanError::Unexpected(e.to_string()))?;

        let mut scan_status = Phase::Running;
        if let Ok(res) = Arc::as_ref(&ov_results.results).lock() {
            scan_status = Phase::from_str(&res.scan_status)
                .map_err(|_| ScanError::Unexpected("Invalid Phase status".to_string()))?;
        }

        match scan_status {
            Phase::Running => {
                return Err(ScanError::Unexpected(format!(
                    "Not allowed to delete a running scan {}",
                    scan_id
                )))
            }
            _ => match self.remove_running(scan_id) {
                Some(_) => {
                    redis_help
                        .release()
                        .map_err(|e| ScanError::Unexpected(e.to_string()))?;
                    tracing::debug!("Scan {scan_id} delete successfully");
                    Ok(())
                }
                None => return Err(OpenvasError::ScanNotFound(scan_id.to_string()).into()),
            },
        }
    }
}

#[async_trait]
impl ScanResultFetcher for Scanner {
    /// Fetches the results of a scan and combines the results with response
    async fn fetch_results<I>(&self, id: I) -> Result<ScanResults, ScanError>
    where
        I: AsRef<str> + Send + 'static,
    {
        let scan_id = id.as_ref();

        let dbid = match self
            .running
            .lock()
            .map_err(|e| ScanError::Unexpected(e.to_string()))?
            .get(scan_id)
        {
            Some(scan) => scan.1,
            None => return Err(OpenvasError::ScanNotFound(scan_id.to_string()).into()),
        };

        let mut redis_help = self.create_redis_connector(Some(dbid));
        let mut ov_results = ResultHelper::init(&mut redis_help);

        ov_results
            .collect_results()
            .await
            .map_err(|e| ScanError::Unexpected(e.to_string()))?;
        ov_results
            .collect_host_status()
            .await
            .map_err(|e| ScanError::Unexpected(e.to_string()))?;
        ov_results
            .collect_scan_status(scan_id.to_string())
            .await
            .map_err(|e| ScanError::Unexpected(e.to_string()))?;

        match Arc::as_ref(&ov_results.results).lock() {
            Ok(all_results) => {
                //let all_results = all_r.clone();
                let hosts_info = HostInfo {
                    all: all_results.count_total as u32,
                    excluded: all_results.count_excluded as u32,
                    dead: all_results.count_dead as u32,
                    alive: all_results.count_alive as u32,
                    queued: 0,
                    finished: all_results.count_alive as u32,
                    scanning: Some(all_results.host_status.clone()),
                };

                let st = Status {
                    start_time: None,
                    end_time: None,
                    status: Phase::from_str(&all_results.scan_status)
                        .map_err(|_| ScanError::Unexpected("Invalid Phase status".to_string()))?,
                    host_info: Some(hosts_info),
                };

                let scan_res = ScanResults {
                    id: scan_id.to_string(),
                    status: st,
                    results: all_results
                        .results
                        .iter()
                        .map(|r| models::Result::from(r).clone())
                        .collect(),
                };

                return Ok(scan_res);
            }
            Err(_) => return Err(OpenvasError::ScanNotFound(scan_id.to_string()).into()),
        };
    }
}

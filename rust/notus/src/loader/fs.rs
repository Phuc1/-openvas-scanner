// SPDX-FileCopyrightText: 2023 Greenbone AG
//
// SPDX-License-Identifier: GPL-2.0-or-later

use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
    time::SystemTime,
};

use models::Advisories;

use crate::error::Error;
use feed::SignatureChecker;

use super::{AdvisoriesLoader, FeedStamp};

#[derive(Debug, Clone)]
pub struct FSAdvisoryLoader<P>
where
    P: AsRef<Path>,
{
    root: P,
}

impl<P> FSAdvisoryLoader<P>
where
    P: AsRef<Path>,
{
    pub fn new(path: P) -> Result<Self, Error> {
        if !path.as_ref().exists() {
            return Err(Error::MissingAdvisoryDir(
                path.as_ref().to_string_lossy().to_string(),
            ));
        }

        if !path.as_ref().is_dir() {
            return Err(Error::AdvisoryDirIsFile(
                path.as_ref().to_string_lossy().to_string(),
            ));
        }

        Ok(Self { root: path })
    }
}

impl<P> SignatureChecker for FSAdvisoryLoader<P>
where
    P: AsRef<Path>,
{}

impl<P> AdvisoriesLoader for FSAdvisoryLoader<P>
where
    P: AsRef<Path>,
{
    fn load_package_advisories(&self, os: &str) -> Result<(Advisories, FeedStamp), Error> {
        let notus_file = self.root.as_ref().join(format!("{os}.notus"));
        let notus_file_str = notus_file.to_string_lossy().to_string();
        let mut file = match File::open(notus_file) {
            Ok(file) => file,
            Err(err) => {
                if matches!(err.kind(), io::ErrorKind::NotFound) {
                    return Err(Error::UnknownOs(os.to_string()));
                }
                return Err(Error::LoadAdvisoryError(
                    notus_file_str,
                    crate::error::LoadAdvisoryErrorKind::IOError(err),
                ));
            }
        };
        let mut buf = String::new();
        if let Err(err) = file.read_to_string(&mut buf) {
            return Err(Error::LoadAdvisoryError(
                notus_file_str,
                crate::error::LoadAdvisoryErrorKind::IOError(err),
            ));
        }
        let mod_time = match file.metadata() {
            Ok(metadata) => match metadata.modified() {
                Ok(time) => time,
                Err(_) => SystemTime::UNIX_EPOCH,
            },
            Err(_) => SystemTime::UNIX_EPOCH,
        };
        match serde_json::from_str(&buf) {
            Ok(adv) => Ok((adv, FeedStamp::Time(mod_time))),
            Err(err) => Err(Error::JSONParseError(notus_file_str, err)),
        }
    }

    fn get_available_os(&self) -> Result<Vec<String>, Error> {
        let paths = fs::read_dir(self.root.as_ref()).map_err(|err| {
            Error::UnreadableAdvisoryDir(self.root.as_ref().to_string_lossy().to_string(), err)
        })?;

        let mut available_os = vec![];
        for dir_entry in paths.flatten() {
            let file = dir_entry.path();
            if file.is_file() {
                if let Some(p) = file.file_name() {
                    if p.to_string_lossy().ends_with(".notus") {
                        if let Some(stem) = file.file_stem() {
                            available_os.push(stem.to_string_lossy().to_string());
                        }
                    }
                }
            }
        }
        Ok(available_os)
    }

    fn has_changed(&self, os: &str, stamp: &FeedStamp) -> bool {
        let notus_file = self.root.as_ref().join(format!("{os}.notus"));

        if let Ok(file) = File::open(notus_file) {
            let mod_time = match file.metadata() {
                Ok(metadata) => match metadata.modified() {
                    Ok(time) => time,
                    Err(_) => return false,
                },
                Err(_) => return false,
            };

            return *stamp != FeedStamp::Time(mod_time);
        }

        false
    }

    /// Perform a signature check of the sha256sums file
    fn verify_signature(&self) -> Result<(), feed::VerifyError> {
        let p = self.root.as_ref().to_str().unwrap_or_default();
        <FSAdvisoryLoader<P> as self::SignatureChecker>::signature_check(p)
    }
    /// Get the notus products root directory
    fn get_root_dir(&self) -> Result<String, Error> {
        let path = self.root.as_ref().to_str().unwrap();
        Ok(path.to_string())
    }

}

#[cfg(test)]
mod tests {

    use crate::{error::Error, loader::AdvisoriesLoader};

    use super::FSAdvisoryLoader;

    #[test]
    fn test_load_advisories() {
        let mut path = env!("CARGO_MANIFEST_DIR").to_string();
        path.push_str("/data");
        let loader = FSAdvisoryLoader::new(path).unwrap();
        let _ = loader.load_package_advisories("debian_10").unwrap();
    }

    #[test]
    fn test_err_missing_advisory_dir() {
        let mut path = env!("CARGO_MANIFEST_DIR").to_string();
        path.push_str("/data_foo");
        assert!(
            matches!(FSAdvisoryLoader::new(path.clone()).expect_err("Should fail"), Error::MissingAdvisoryDir(p) if p == path)
        );
    }

    #[test]
    fn test_err_advisory_dir_is_file() {
        let mut path = env!("CARGO_MANIFEST_DIR").to_string();
        path.push_str("/data/debian_10.notus");
        assert!(
            matches!(FSAdvisoryLoader::new(path.clone()).expect_err("Should fail"), Error::AdvisoryDirIsFile(p) if p == path)
        );
    }

    #[test]
    fn test_err_unknown_os() {
        let mut path = env!("CARGO_MANIFEST_DIR").to_string();
        path.push_str("/data");
        let loader = FSAdvisoryLoader::new(path).unwrap();

        let os = "foo";
        assert!(
            matches!(loader.load_package_advisories(os).expect_err("Should fail"), Error::UnknownOs(o) if o == os)
        );
    }

    #[test]
    fn test_err_json_parse() {
        let mut path = env!("CARGO_MANIFEST_DIR").to_string();
        path.push_str("/data");
        let loader = FSAdvisoryLoader::new(path.clone()).unwrap();

        let os = "debian_10_json_parse_err";
        assert!(
            matches!(loader.load_package_advisories(os).expect_err("Should fail"), Error::JSONParseError(p, _) if p == format!("{path}/{os}.notus"))
        );
    }

    #[test]
    fn test_available_os() {
        let mut path = env!("CARGO_MANIFEST_DIR").to_string();
        path.push_str("/data");
        let loader = FSAdvisoryLoader::new(path.clone()).unwrap();

        let available_os = loader.get_available_os().unwrap();

        assert_eq!(available_os.len(), 3);
        assert!(available_os.contains(&"debian_10".to_string()));
        assert!(available_os.contains(&"debian_10_json_parse_err".to_string()));
        assert!(available_os.contains(&"debian_10_advisory_parse_err".to_string()));
    }
}

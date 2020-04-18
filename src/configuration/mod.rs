use std::fs::{File, OpenOptions};
use std::io::prelude::*;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use anyhow::{anyhow, Context, Error, Result};
use jsonwebtoken::EncodingKey;
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use structopt::StructOpt;
use url::Host;
use url::Url;

#[derive(Debug)]
struct EnvironmentConfiguration {
    /// The full address to run the server on
    server_addr: Option<Url>,
    /// Database URL (will distinguish between postgres, sqlite, sled)
    database_addr: Option<Url>,
    /// Path to PEM encoded ES256 key for creating auth tokens
    authkey_path: Option<PathBuf>,
    /// Duration in seconds that an auth token is valid for
    session_expiration: Option<u64>,
    /// Server configuration file location
    configuration_path: Option<PathBuf>,
}

#[derive(Debug, StructOpt)]
struct CliConfiguration {
    /// The full address to run the server on
    server_addr: Option<Url>,
    /// Database URL (will distinguish between postgres, sqlite, sled)
    database_addr: Option<Url>,
    /// Path to PEM encoded ES256 key for creating auth tokens
    authkey_path: Option<PathBuf>,
    /// Duration in seconds that an auth token is valid for
    session_expiration: Option<u64>,
    /// Server configuration file location
    configuration_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct YamlConfiguration {
    /// The full address to run the server on
    server_addr: Option<Url>,
    /// Database URL (will distinguish between postgres, sqlite, sled)
    database_addr: Option<Url>,
    /// Path to PEM encoded ES256 key for creating auth tokens
    authkey_path: Option<PathBuf>,
    /// Duration in seconds that an auth token is valid for
    session_expiration: Option<u64>,
}

/// Combined server configuration generated by layering all 3 configuration methods
/// Follows a simple priority system of env -> cli args -> config file when initialized
/// Will fail to initialize if the 3 configuration methods combined miss a required option
#[derive(Debug, Deserialize, Serialize)]
struct LayeredServerConfiguration {
    /// The full address to run the server on
    server_addr: Url,
    /// Database URL (will distinguish between postgres, sqlite, sled)
    database_addr: Url,
    /// PEM encoded ES256 key for creating auth tokens
    authkey_path: PathBuf,
    /// Duration in seconds that an auth token is valid for
    /// Optional
    session_expiration: Duration,
}

#[derive(Debug)]
/// Unable Configuration struct that contains all relevant configuration information in accessible fields/types
/// Made from a LayeredServerConfiguration.
pub struct ServerConfiguration {
    /// The full address to run the server on
    pub server_addr: Url,
    /// Database URL (will distinguish between postgres, sqlite, sled)
    pub database_addr: Url,
    /// PEM encoded ES256 key for creating auth tokens
    pub authkey: EncodingKey,
    /// Duration in seconds that an auth token is valid for
    /// Optional
    pub session_expiration: Duration,
}

impl EnvironmentConfiguration {
    fn new() -> Self {
        let server_addr = match std::env::var("MAELSTROM_SERVER_ADDRESS") {
            Ok(v) => match Url::parse(&v) {
                Ok(v) => Some(v),
                // TODO: Fail out if we get this far, since we assume you want to use the envvar rather than some lower
                // priority configuration as part of the layers
                Err(_) => None,
            },
            Err(_) => None,
        };
        let database_addr = match std::env::var("MAELSTROM_DATABASE_ADDRESS") {
            Ok(v) => match Url::parse(&v) {
                Ok(v) => Some(v),
                // TODO: Fail out if we get this far, since we assume you want to use the envvar rather than some lower
                // priority configuration as part of the layers
                Err(_) => None,
            },
            Err(_) => None,
        };
        let authkey_path = match std::env::var("MAELSTROM_AUTHKEY_PATH") {
            Ok(v) => Some(PathBuf::from(&v)),
            Err(_) => None,
        };
        let session_expiration = match std::env::var("MAELSTROM_SESSION_EXPIRATION") {
            Ok(v) => match v.parse() {
                Ok(v) => Some(v),
                // TODO: Fail out if we get this far, since we assume you want to use the envvar rather than some lower
                // priority configuration as part of the layers
                Err(_) => None,
            },
            Err(_) => None,
        };
        let configuration_path = match std::env::var("MAELSTROM_CONF_PATH") {
            Ok(v) => Some(PathBuf::from(&v)),
            Err(_) => None,
        };

        Self {
            server_addr,
            database_addr,
            authkey_path,
            session_expiration,
            configuration_path,
        }
    }
}

impl YamlConfiguration {
    fn default() -> Self {
        let yaml = Self {
            server_addr: Some(Url::parse("https://example.net").unwrap()),
            database_addr: Some(Url::parse("postgres://db.example.net").unwrap()),
            authkey_path: Some(PathBuf::from("/etc/maelstrom/authkey.pem")),
            session_expiration: Some(3000),
        };
        yaml
    }

    fn load() -> Self {
        unimplemented!()
    }

    fn save(&self, path: &PathBuf) -> Result<(), Error> {
        let s = serde_yaml::to_string(self).with_context(|| {
            format!("Failed to serilize to yaml. Provided struct is {:?}", self)
        })?;
        info!("Saved yaml configuration file");
        debug!("Saved yaml looks like: {:?}", s);
        match OpenOptions::new().write(true).create(true).open(&path) {
            Ok(mut v) => {
                v.write_all(s.as_bytes())?;
                Ok(())
            }
            Err(e) => Err(anyhow!(
                "Unable to open file for writing. Reason is {:?}",
                e
            )),
        }
    }
}

impl LayeredServerConfiguration {
    fn new() -> Self {
        let env = EnvironmentConfiguration::new();
        let cli = CliConfiguration::from_args();
        let yaml_path = match env.configuration_path {
            Some(v) => v,
            None => match cli.configuration_path {
                Some(v) => v,
                None => {
                    error!("No configuration path specified. This argument is required!");
                    exit(1) // TODO: Determine proper "standardized" exit code for missing arguments
                }
            },
        };
        let yaml = match File::open(&yaml_path) {
            Ok(v) => {
                let rdr = BufReader::new(v);
                match serde_yaml::from_reader(rdr) {
                    Ok(v) => v,
                    Err(e) => {
                        error!("Unable to read yaml file. Reason is {:?}", e);
                        exit(1)
                    }
                }
            }
            Err(e) => match e.kind() {
                std::io::ErrorKind::NotFound => {
                    let yaml = YamlConfiguration::default();
                    warn!("No yaml file found. Creating default yaml file and writing to disk. If this is a first run, exit and edit before continuing");
                    debug!("Default yaml looks like: {:?}", yaml);
                    match yaml.save(&yaml_path) {
                        Ok(()) => yaml,
                        Err(e) => {
                            error!("Unable to write default yaml file. This is required! Error is {:?}", e);
                            exit(1)
                        }
                    }
                }
                _ => {
                    error!("Unable to handle error {:?}", e);
                    exit(1)
                }
            },
        };
        Self {
            server_addr: match env.server_addr {
                Some(v) => v,
                None => match cli.server_addr {
                    Some(v) => v,
                    None => match yaml.server_addr {
                        Some(v) => v,
                        None => {
                            error!("Option server_addr is required!");
                            exit(1)
                        }
                    },
                },
            },
            database_addr: match env.database_addr {
                Some(v) => v,
                None => match cli.database_addr {
                    Some(v) => v,
                    None => match yaml.database_addr {
                        Some(v) => v,
                        None => {
                            error!("Option database_addr is required!");
                            exit(1)
                        }
                    },
                },
            },
            authkey_path: match env.authkey_path {
                Some(v) => v,
                None => match cli.authkey_path {
                    Some(v) => v,
                    None => match yaml.authkey_path {
                        Some(v) => v,
                        None => {
                            error!("Option authkey_path is required!");
                            exit(1)
                        }
                    },
                },
            },
            session_expiration: match env.session_expiration {
                Some(v) => Duration::from_secs(v),
                None => match cli.session_expiration {
                    Some(v) => Duration::from_secs(v),
                    None => match yaml.session_expiration {
                        Some(v) => Duration::from_secs(v),
                        None => Duration::from_secs(60),
                    },
                },
            },
        }
    }
}

impl ServerConfiguration {
    fn new() -> Self {
        let layered_configuration = LayeredServerConfiguration::new();
        Self {
            server_addr: layered_configuration.server_addr,
            database_addr: layered_configuration.database_addr,
            authkey: match File::open(layered_configuration.authkey_path) {
                Ok(mut v) => {
                    let mut key = match &v.metadata() {
                        Ok(v) => Vec::<u8>::with_capacity(v.len() as usize),
                        Err(e) => unimplemented!(),
                    };
                    match v.read_to_end(&mut key) {
                        Ok(_) => match EncodingKey::from_ec_pem(&key) {
                            Ok(v) => v,
                            Err(e) => {
                                error!("Unable to parse supplied key. Reason is {:?}", e);
                                exit(1)
                            }
                        },
                        Err(e) => {
                            error!("Unable to read key file. Reason is {:?}", e);
                            exit(1)
                        }
                    }
                }
                Err(e) => {
                    error!("Unable to open authkey file. Reason is {:?}", e);
                    exit(1)
                }
            },
            session_expiration: layered_configuration.session_expiration,
        }
    }
}

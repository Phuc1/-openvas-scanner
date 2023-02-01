// Copyright (C) 2023 Greenbone Networks GmbH
//
// SPDX-License-Identifier: GPL-2.0-or-later

use std::{
    fs, io,
    path::{Path, PathBuf},
    process,
};

use clap::{Parser, Subcommand};
use configparser::ini::Ini;
use nasl_interpreter::{ContextType, FSPluginLoader, Interpreter, Register};
use nasl_syntax::{Statement, SyntaxError};
use redis_sink::connector::RedisCache;
use sink::{DefaultSink, Sink};
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Subcommand to print the raw statements of a file.
    ///
    /// It is mostly for debug purposes and verification if the nasl-syntax-parser is working as expected.
    Syntax {
        /// The path for the file or dir to parse
        #[arg(short, long)]
        path: PathBuf,
        /// prints the parsed statements
        #[arg(short, long, default_value_t = false)]
        verbose: bool,
    },
    /// Subcommand to print the raw statements of a file.
    ///
    /// It is mostly for debug purposes and verification if the nasl-syntax-parser is working as expected.
    Feed {
        /// Redis address inform of tcp (redis://) or unix socket (unix://).
        ///
        /// It must be the complete redis address in either the form of a unix socket or tcp.
        /// For tcp provide the address in the form of: `redis://host:port`.
        /// For unix sockket provide the path to the socket in the form of: `unix://path/to/redis.sock`.
        /// When not provided the DefaultSink will be used instead.
        #[arg(short, long)]
        redis: Option<String>,
        /// The path to the NASL plugins
        #[arg(short, long)]
        path: Option<PathBuf>,
        /// prints the parsed statements
        #[arg(short, long, default_value_t = false)]
        verbose: bool,
        #[command(subcommand)]
        action: FeedAction,
    },
}

#[derive(clap::Subcommand, Debug, Clone)]
enum FeedAction {
    Update,
}

fn load_file<P: AsRef<Path>>(path: P) -> Result<String, io::Error> {
    // unfortunately NASL is not UTF-8 so we need to map it manually
    fs::read(path).map(|bs| bs.iter().map(|&b| b as char).collect())
}

fn read_errors<P: AsRef<Path>>(path: P) -> Result<Vec<SyntaxError>, SyntaxError> {
    let code = load_file(path)?;
    Ok(nasl_syntax::parse(&code)
        .filter_map(|r| match r {
            Ok(_) => None,
            Err(err) => Some(err),
        })
        .collect())
}

fn read<P: AsRef<Path>>(path: P) -> Result<Vec<Result<Statement, SyntaxError>>, SyntaxError> {
    let code = load_file(path)?;
    Ok(nasl_syntax::parse(&code).collect())
}

fn print_results(path: &Path, verbose: bool) -> usize {
    let mut errors = 0;

    if verbose {
        println!("# {:?}", path);
        let results = read(path).unwrap();
        for r in results {
            match r {
                Ok(stmt) => println!("{:?}", stmt),
                Err(err) => eprintln!("{}", err),
            }
        }
    } else {
        let err = read_errors(&path).unwrap();
        if !err.is_empty() {
            eprintln!("# Error in {:?}", path);
        }
        errors += err.len();
        err.iter().for_each(|r| eprintln!("{}", r));
    }
    errors
}

fn syntax_check(path: PathBuf, verbose: bool) {
    let mut parsed: usize = 0;
    let mut skipped: usize = 0;
    let mut errors: usize = 0;
    println!("verifiying NASL syntax in {:?}.", path);
    if path.as_path().is_dir() {
        for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
            print!("\rparsing {}th file", parsed);
            let ext = {
                if let Some(ext) = entry.path().extension() {
                    ext.to_str().unwrap().to_owned()
                } else {
                    "".to_owned()
                }
            };
            if !matches!(ext.as_str(), "nasl" | "inc") {
                skipped += 1;
            } else {
                errors += print_results(entry.path(), verbose);
                parsed += 1;
            }
        }
        println!();
    } else {
        errors += print_results(path.as_path(), verbose);
        parsed += 1;
    }
    println!(
        "skipped: {} files; parsed: {} files; errors: {}",
        skipped, parsed, errors
    );
}
fn feed_run(storage: &dyn Sink, path: PathBuf, verbose: bool) {
    println!("description run syntax in {:?}.", path);
    if !path.as_path().is_dir() {
        println!("is not a path, stopping.");
        return;
    }
    let root_dir = path.clone();
    let root_dir_len = path.to_str().map(|x| x.len()).unwrap_or_default();
    let loader = FSPluginLoader::new(&root_dir);
    let mut plgin_feed = root_dir.clone();
    plgin_feed.push("plugin_feed_info.inc");

    // load feed version

    let code = load_file(plgin_feed.as_path())
        .unwrap_or_else(|_| panic!("{:?} should be loadable", plgin_feed));
    let mut register = Register::default();
    let mut interpreter = Interpreter::new("WTF", storage, &loader, &mut register);
    nasl_syntax::parse(&code)
        .map(|x| {
            let x = x.expect("don't expect parse error");
            interpreter.resolve(&x).expect("nope")
        })
        .last();
    let feed_version = register
        .named("PLUGIN_SET")
        .map(|x| x.to_string())
        .unwrap_or_else(|| "0".to_owned());
    storage
        .dispatch(
            "generic",
            sink::Dispatch::NVT(sink::nvt::NVTField::Version(feed_version)),
        )
        .unwrap();

    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        let ext = {
            if let Some(ext) = entry.path().extension() {
                ext.to_str().unwrap().to_owned()
            } else {
                "".to_owned()
            }
        };
        if matches!(ext.as_str(), "nasl") {
            let code = load_file(entry.path())
                .unwrap_or_else(|_| panic!("{:?} should be loadable", entry.path()));
            let mut register = Register::root_initial(vec![
                (
                    "description".to_owned(),
                    ContextType::Value(nasl_interpreter::NaslValue::Boolean(true)),
                ),
                (
                    "OPENVAS_VERSION".to_owned(),
                    ContextType::Value(nasl_interpreter::NaslValue::String("1".to_owned())),
                ),
            ]);

            let key = entry.path().to_str().unwrap_or_default();
            let key = &key[root_dir_len..];
            let mut interpreter = Interpreter::new(key, storage, &loader, &mut register);
            let result = nasl_syntax::parse(&code)
                .map(|r| r.unwrap_or_else(|_| panic!(" should be parseable.")))
                .map(|stmt| interpreter.resolve(&stmt))
                .map(|ir| ir.unwrap_or_else(|e| panic!("{e:?}")))
                .find(|ir| matches!(ir, nasl_interpreter::NaslValue::Exit(_)))
                .unwrap();
            storage.on_exit().unwrap();
            if verbose {
                println!("{:?} {:?}.", entry.path(), result);
            }
        }
    }
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Syntax { path, verbose } => syntax_check(path, verbose),
        Command::Feed {
            redis: Some(x),
            path: Some(path),
            verbose,
            action: FeedAction::Update,
        } => {
            let redis = RedisCache::init(&x).unwrap();
            feed_run(&redis, path, verbose)
        }
        Command::Feed {
            redis,
            path,
            verbose,
            action: FeedAction::Update,
        } => {
            // This is only supported as long as nasl-cli does not have an own configuration and is dependent on openvas
            // afterwards we should switch to toml file
            let oconfig = process::Command::new("openvas")
                .arg("-s")
                .output()
                .expect("Unless path and redis url is provided openvas is required.");
            let mut config = Ini::new();
            let oconfig = oconfig.stdout.iter().map(|x| *x as char).collect();
            config
                .read(oconfig)
                .expect("openvas -s must return ini output");
            let redis = {
                if let Some(rp) = redis {
                    rp
                } else {
                    let dba = config
                        .get("default", "db_address")
                        .expect("openvas -s must contain db_address");
                    if dba.starts_with("redis://") || dba.starts_with("unix://") {
                        dba
                    } else {
                        format!("unix://{dba}")
                    }
                }
            };
            let sink = RedisCache::init(&redis).expect("redis connection");
            let path = {
                if let Some(p) = path {
                    p
                } else {
                    PathBuf::from(
                        config
                            .get("default", "plugins_folder")
                            .expect("openvas -s must contain plugins_folder"),
                    )
                }
            };
            feed_run(&sink, path, verbose)
        }
    }
}

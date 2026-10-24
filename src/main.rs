mod cli;
mod config;
mod error;
mod gitguard;
mod identity;
mod process;
mod resource;
mod scan;
mod service;
mod store;
mod supervisor;
mod util;

use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;

fn main() -> ExitCode {
    let mut args: Vec<OsString> = std::env::args_os().collect();
    let invoked_as = args
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    if invoked_as == "git" {
        args.insert(1, OsString::from("git"));
    }
    let cli = Cli::parse_from(args);
    let json = cli.json;
    util::set_state_dir_override(cli.state_dir.clone());
    match cli::dispatch(cli) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(err) => {
            if json {
                let payload = serde_json::json!({
                    "error": {
                        "kind": err.kind(),
                        "message": err.to_string(),
                    }
                });
                eprintln!("{payload}");
            } else {
                eprintln!("arbiter: {err}");
            }
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

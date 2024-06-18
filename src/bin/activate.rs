// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
// SPDX-FileCopyrightText: 2020 Andreas Fuchs <asf@boinkor.net>
// SPDX-FileCopyrightText: 2021 Yannik Sander <contact@ysndr.de>
//
// SPDX-License-Identifier: MPL-2.0

use signal_hook::{consts::signal::SIGHUP, iterator::Signals};

use clap::{Parser, Subcommand};

use tokio::fs;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::timeout;

use std::time::Duration;

use std::env;
use std::path::{Path, PathBuf};

use notify::{recommended_watcher, RecommendedWatcher, RecursiveMode, Watcher};

use thiserror::Error;

use log::{debug, error, info, warn};

/// Remote activation utility for deploy-rs
#[derive(Parser, Debug)]
#[clap(version = "1.0", author = "Serokell <https://serokell.io/>")]
struct Opts {
    /// Print debug logs to output
    #[clap(short, long)]
    debug_logs: bool,
    /// Directory to print logs to
    #[clap(long)]
    log_dir: Option<String>,

    #[command(subcommand)]
    subcmd: SubCmd,
}

#[derive(Subcommand, Debug)]
enum SubCmd {
    Activate {
        /// The closure to activate
        closure: String,
        /// The profile path to install into
        #[clap(long)]
        profile_path: Option<String>,
        /// The profile user if explicit profile path is not specified
        #[clap(long, requires = "profile-name")]
        profile_user: Option<String>,
        /// The profile name
        #[clap(long, requires = "profile-user")]
        profile_name: Option<String>,

        /// Maximum time to wait for confirmation after activation
        #[clap(long)]
        confirm_timeout: u16,

        /// Wait for confirmation after deployment and rollback if not confirmed
        #[clap(long)]
        magic_rollback: bool,

        /// Auto rollback if failure
        #[clap(long)]
        auto_rollback: bool,

        /// Show what will be activated on the machines
        #[clap(long)]
        dry_activate: bool,

        /// Don't activate, but update the boot loader to boot into the new profile
        #[clap(long)]
        boot: bool,

        /// Path for any temporary files that may be needed during activation
        #[clap(long)]
        temp_path: PathBuf,
    },
    Wait {
        /// The closure to wait for
        closure: String,

        /// Path for any temporary files that may be needed during activation
        #[arg(long)]
        temp_path: PathBuf,

        /// Timeout to wait for activation
        #[clap(long)]
        activation_timeout: Option<u16>,
    },
    Revoke {
        /// The profile path to install into
        #[clap(long)]
        profile_path: Option<String>,
        /// The profile user if explicit profile path is not specified
        #[clap(long, requires = "profile-name")]
        profile_user: Option<String>,
        /// The profile name
        #[clap(long, requires = "profile-user")]
        profile_name: Option<String>,
    },
}

#[derive(Error, Debug)]
pub enum DeactivateError {
    #[error("Failed to execute the rollback command: {0}")]
    Rollback(std::io::Error),
    #[error("The rollback resulted in a bad exit code: {0:?}")]
    RollbackExit(Option<i32>),
    #[error("Failed to run command for listing generations: {0}")]
    ListGen(std::io::Error),
    #[error("Command for listing generations resulted in a bad exit code: {0:?}")]
    ListGenExit(Option<i32>),
    #[error("Error converting generation list output to utf8: {0}")]
    DecodeListGenUtf8(std::string::FromUtf8Error),
    #[error("Failed to run command for deleting generation: {0}")]
    DeleteGen(std::io::Error),
    #[error("Command for deleting generations resulted in a bad exit code: {0:?}")]
    DeleteGenExit(Option<i32>),
    #[error("Failed to run command for re-activating the last generation: {0}")]
    Reactivate(std::io::Error),
    #[error("Command for re-activating the last generation resulted in a bad exit code: {0:?}")]
    ReactivateExit(Option<i32>),
}

pub async fn deactivate(profile_path: &str) -> Result<(), DeactivateError> {
    warn!("De-activating due to error");

    let nix_env_rollback_exit_status = Command::new("nix-env")
        .arg("-p")
        .arg(profile_path)
        .arg("--rollback")
        .status()
        .await
        .map_err(DeactivateError::Rollback)?;

    match nix_env_rollback_exit_status.code() {
        Some(0) => (),
        a => return Err(DeactivateError::RollbackExit(a)),
    };

    debug!("Listing generations");

    let nix_env_list_generations_out = Command::new("nix-env")
        .arg("-p")
        .arg(profile_path)
        .arg("--list-generations")
        .output()
        .await
        .map_err(DeactivateError::ListGen)?;

    match nix_env_list_generations_out.status.code() {
        Some(0) => (),
        a => return Err(DeactivateError::ListGenExit(a)),
    };

    let generations_list = String::from_utf8(nix_env_list_generations_out.stdout)
        .map_err(DeactivateError::DecodeListGenUtf8)?;

    let last_generation_line = generations_list
        .lines()
        .last()
        .expect("Expected to find a generation in list");

    let last_generation_id = last_generation_line
        .split_whitespace()
        .next()
        .expect("Expected to get ID from generation entry");

    debug!("Removing generation entry {}", last_generation_line);
    warn!("Removing generation by ID {}", last_generation_id);

    let nix_env_delete_generation_exit_status = Command::new("nix-env")
        .arg("-p")
        .arg(profile_path)
        .arg("--delete-generations")
        .arg(last_generation_id)
        .status()
        .await
        .map_err(DeactivateError::DeleteGen)?;

    match nix_env_delete_generation_exit_status.code() {
        Some(0) => (),
        a => return Err(DeactivateError::DeleteGenExit(a)),
    };

    info!("Attempting to re-activate the last generation");

    let re_activate_exit_status = Command::new(format!("{}/deploy-rs-activate", profile_path))
        .env("PROFILE", profile_path)
        .current_dir(profile_path)
        .status()
        .await
        .map_err(DeactivateError::Reactivate)?;

    match re_activate_exit_status.code() {
        Some(0) => (),
        a => return Err(DeactivateError::ReactivateExit(a)),
    };

    Ok(())
}

#[derive(Error, Debug)]
pub enum ActivationConfirmationError {
    #[error("Failed to create activation confirmation directory: {0}")]
    CreateConfirmDir(std::io::Error),
    #[error("Failed to create activation confirmation file: {0}")]
    CreateConfirmFile(std::io::Error),
    #[error("Could not watch for activation sentinel: {0}")]
    Watcher(#[from] notify::Error),
    #[error("Error waiting for confirmation event: {0}")]
    WaitingError(#[from] DangerZoneError),
}

#[derive(Error, Debug)]
pub enum DangerZoneError {
    #[error("Timeout elapsed for confirmation")]
    TimesUp,
    #[error("inotify stream ended without activation confirmation")]
    NoConfirmation,
    #[error("inotify encountered an error: {0}")]
    Watch(notify::Error),
}

async fn danger_zone(
    mut events: mpsc::Receiver<Result<(), notify::Error>>,
    confirm_timeout: u16,
) -> Result<(), DangerZoneError> {
    info!("Waiting for confirmation event...");

    match timeout(Duration::from_secs(confirm_timeout as u64), events.recv()).await {
        Ok(Some(Ok(()))) => Ok(()),
        Ok(Some(Err(e))) => Err(DangerZoneError::Watch(e)),
        Ok(None) => Err(DangerZoneError::NoConfirmation),
        Err(_) => Err(DangerZoneError::TimesUp),
    }
}

pub async fn activation_confirmation(
    temp_path: PathBuf,
    confirm_timeout: u16,
    closure: String,
) -> Result<(), ActivationConfirmationError> {
    let lock_path = deploy::make_lock_path(&temp_path, &closure);

    debug!("Ensuring parent directory exists for canary file");

    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(ActivationConfirmationError::CreateConfirmDir)?;
    }

    debug!("Creating canary file");

    fs::File::create(&lock_path)
        .await
        .map_err(ActivationConfirmationError::CreateConfirmFile)?;

    debug!("Creating notify watcher");

    let (deleted, done) = mpsc::channel(1);

    let mut watcher: RecommendedWatcher =
        recommended_watcher(move |res: Result<notify::event::Event, notify::Error>| {
            let send_result = match res {
                Ok(e) if e.kind == notify::EventKind::Remove(notify::event::RemoveKind::File) => {
                    debug!("Got worthy removal event, sending on channel");
                    deleted.try_send(Ok(()))
                }
                Err(e) => {
                    debug!("Got error waiting for removal event, sending on channel");
                    deleted.try_send(Err(e))
                }
                Ok(_) => Ok(()), // ignore non-removal events
            };

            if let Err(e) = send_result {
                error!("Could not send file system event to watcher: {}", e);
            }
        })?;

    watcher.watch(&lock_path, RecursiveMode::NonRecursive)?;

    danger_zone(done, confirm_timeout)
        .await
        .map_err(ActivationConfirmationError::WaitingError)
}

#[derive(Error, Debug)]
pub enum WaitError {
    #[error("Error creating watcher for activation: {0}")]
    Watcher(#[from] notify::Error),
    #[error("Error waiting for activation: {0}")]
    Waiting(#[from] DangerZoneError),
}
pub async fn wait(
    temp_path: PathBuf,
    closure: String,
    activation_timeout: Option<u16>,
) -> Result<(), WaitError> {
    let lock_path = deploy::make_lock_path(&temp_path, &closure);

    let (created, done) = mpsc::channel(1);

    let mut watcher: RecommendedWatcher = {
        // TODO: fix wasteful clone
        let lock_path = lock_path.clone();

        recommended_watcher(move |res: Result<notify::event::Event, notify::Error>| {
            let send_result = match res {
                Ok(e) if e.kind == notify::EventKind::Create(notify::event::CreateKind::File) => {
                    match &e.paths[..] {
                        [x] => match lock_path.canonicalize() {
                            // 'lock_path' may not exist yet when some other files are created in 'temp_path'
                            // x is already supposed to be canonical path
                            Ok(lock_path) if x == &lock_path => created.try_send(Ok(())),
                            _ => Ok(()),
                        },
                        _ => Ok(()),
                    }
                }
                Err(e) => created.try_send(Err(e)),
                Ok(_) => Ok(()), // ignore non-removal events
            };

            if let Err(e) = send_result {
                error!("Could not send file system event to watcher: {}", e);
            }
        })?
    };

    watcher.watch(&temp_path, RecursiveMode::NonRecursive)?;

    // Avoid a potential race condition by checking for existence after watcher creation
    if fs::metadata(&lock_path).await.is_ok() {
        watcher.unwatch(&temp_path)?;
        return Ok(());
    }

    danger_zone(done, activation_timeout.unwrap_or(240)).await?;

    info!("Found canary file, done waiting!");

    Ok(())
}

#[derive(Error, Debug)]
pub enum ActivateError {
    #[error("Failed to execute the command for setting profile: {0}")]
    SetProfile(std::io::Error),
    #[error("The command for setting profile resulted in a bad exit code: {0:?}")]
    SetProfileExit(Option<i32>),

    #[error("Failed to execute the activation script: {0}")]
    RunActivate(std::io::Error),
    #[error("The activation script resulted in a bad exit code: {0:?}")]
    RunActivateExit(Option<i32>),

    #[error("There was an error de-activating after an error was encountered: {0}")]
    Deactivate(#[from] DeactivateError),

    #[error("Failed to get activation confirmation: {0}")]
    ActivationConfirmation(#[from] ActivationConfirmationError),
}

#[allow(clippy::too_many_arguments)]
pub async fn activate(
    profile_path: String,
    closure: String,
    auto_rollback: bool,
    temp_path: PathBuf,
    confirm_timeout: u16,
    magic_rollback: bool,
    dry_activate: bool,
    boot: bool,
) -> Result<(), ActivateError> {
    if !dry_activate {
        info!("Activating profile");
        let nix_env_set_exit_status = Command::new("nix-env")
            .arg("-p")
            .arg(&profile_path)
            .arg("--set")
            .arg(&closure)
            .status()
            .await
            .map_err(ActivateError::SetProfile)?;
        match nix_env_set_exit_status.code() {
            Some(0) => (),
            a => {
                if auto_rollback && !dry_activate {
                    deactivate(&profile_path).await?;
                }
                return Err(ActivateError::SetProfileExit(a));
            }
        };
    }

    debug!("Running activation script");

    let activation_location = if dry_activate {
        &closure
    } else {
        &profile_path
    };

    let activate_status = match Command::new(format!("{}/deploy-rs-activate", activation_location))
        .env("PROFILE", activation_location)
        .env("DRY_ACTIVATE", if dry_activate { "1" } else { "0" })
        .env("BOOT", if boot { "1" } else { "0" })
        .current_dir(activation_location)
        .status()
        .await
        .map_err(ActivateError::RunActivate)
    {
        Ok(x) => x,
        Err(e) => {
            if auto_rollback && !dry_activate {
                deactivate(&profile_path).await?;
            }
            return Err(e);
        }
    };

    if !dry_activate {
        match activate_status.code() {
            Some(0) => (),
            a => {
                if auto_rollback {
                    deactivate(&profile_path).await?;
                }
                return Err(ActivateError::RunActivateExit(a));
            }
        };

        if !dry_activate {
            info!("Activation succeeded!");
        }

        if magic_rollback && !boot {
            info!("Magic rollback is enabled, setting up confirmation hook...");
            if let Err(err) = activation_confirmation(temp_path, confirm_timeout, closure).await {
                deactivate(&profile_path).await?;
                return Err(ActivateError::ActivationConfirmation(err));
            }
        }
    }

    Ok(())
}

async fn revoke(profile_path: String) -> Result<(), DeactivateError> {
    deactivate(profile_path.as_str()).await?;
    Ok(())
}

#[derive(Error, Debug)]
pub enum GetProfilePathError {
    #[error("Failed to deduce HOME directory for user {0}")]
    NoUserHome(String),
}

fn get_profile_path(
    profile_path: Option<String>,
    profile_user: Option<String>,
    profile_name: Option<String>,
) -> Result<String, GetProfilePathError> {
    match (profile_path, profile_user, profile_name) {
        (Some(profile_path), None, None) => Ok(profile_path),
        (None, Some(profile_user), Some(profile_name)) => {
            let nix_state_dir = env::var("NIX_STATE_DIR").unwrap_or("/nix/var/nix".to_string());
            // As per https://nixos.org/manual/nix/stable/command-ref/files/profiles#profiles
            match &profile_user[..] {
                "root" => {
                    match &profile_name[..] {
                        // NixOS system profile belongs to the root user, but isn't stored in the 'per-user/root'
                        "system" => Ok(format!("{}/profiles/system", nix_state_dir)),
                        _ => Ok(format!(
                            "{}/profiles/per-user/root/{}",
                            nix_state_dir, profile_name
                        )),
                    }
                }
                _ => {
                    let old_user_profiles_dir =
                        format!("{}/profiles/per-user/{}", nix_state_dir, profile_user);
                    // To stay backward compatible
                    if Path::new(&old_user_profiles_dir).exists() {
                        Ok(format!("{}/{}", old_user_profiles_dir, profile_name))
                    } else {
                        // https://github.com/NixOS/nix/blob/2.17.0/src/libstore/profiles.cc#L308
                        // This is basically the equivalent of calling 'dirs::state_dir()'.
                        // However, this function returns 'None' on macOS, while nix will actually
                        // check env variables, so we imitate nix implementation below instead of
                        // using 'dirs::state_dir()' directly.
                        let state_dir = env::var("XDG_STATE_HOME").or_else(|_| {
                            dirs::home_dir()
                                .map(|h| format!("{}/.local/state", h.as_path().display()))
                                .ok_or(GetProfilePathError::NoUserHome(profile_user))
                        })?;
                        Ok(format!("{}/nix/profiles/{}", state_dir, profile_name))
                    }
                }
            }
        }
        _ => panic!("impossible"),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Ensure that this process stays alive after the SSH connection dies
    let mut signals = Signals::new([SIGHUP])?;
    std::thread::spawn(move || {
        for _ in signals.forever() {
            println!("Received SIGHUP - ignoring...");
        }
    });

    let opts: Opts = Opts::parse();

    let _handle = deploy::init_logger(
        opts.debug_logs,
        opts.log_dir.as_deref(),
        &match opts.subcmd {
            SubCmd::Activate { .. } => deploy::LoggerType::Activate,
            SubCmd::Wait { .. } => deploy::LoggerType::Wait,
            SubCmd::Revoke { .. } => deploy::LoggerType::Revoke,
        },
    )?;

    let r = match opts.subcmd {
        SubCmd::Activate {
            closure,
            profile_path,
            profile_user,
            profile_name,
            confirm_timeout,
            magic_rollback,
            auto_rollback,
            dry_activate,
            boot,
            temp_path,
        } => activate(
            get_profile_path(profile_path, profile_user, profile_name)?,
            closure,
            auto_rollback,
            temp_path,
            confirm_timeout,
            magic_rollback,
            dry_activate,
            boot,
        )
        .await
        .map_err(|x| Box::new(x) as Box<dyn std::error::Error>),

        SubCmd::Wait {
            closure,
            temp_path,
            activation_timeout,
        } => wait(temp_path, closure, activation_timeout)
            .await
            .map_err(|x| Box::new(x) as Box<dyn std::error::Error>),

        SubCmd::Revoke {
            profile_path,
            profile_user,
            profile_name,
        } => revoke(get_profile_path(profile_path, profile_user, profile_name)?)
            .await
            .map_err(|x| Box::new(x) as Box<dyn std::error::Error>),
    };

    match r {
        Ok(()) => (),
        Err(err) => {
            error!("{}", err);
            std::process::exit(1)
        }
    }

    Ok(())
}

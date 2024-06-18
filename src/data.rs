// SPDX-FileCopyrightText: 2020 Serokell <https://serokell.io/>
//
// SPDX-License-Identifier: MPL-2.0

// silence clippy warnings about the dejson macro
#![allow(clippy::question_mark)]

use merge::Merge;
use std::collections::HashMap;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone, Merge)]
#[merge(strategy = merge::option::overwrite_none)]
pub struct GenericSettings {
    #[serde(rename = "sshUser")]
    pub ssh_user: Option<String>,
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default, rename = "sshOpts")]
    #[merge(strategy = merge::vec::append)]
    pub ssh_opts: Vec<String>,
    #[serde(rename = "fastConnection")]
    pub fast_connection: Option<bool>,
    #[serde(rename = "autoRollback")]
    pub auto_rollback: Option<bool>,
    #[serde(rename = "confirmTimeout")]
    pub confirm_timeout: Option<u16>,
    #[serde(rename = "activationTimeout")]
    pub activation_timeout: Option<u16>,
    #[serde(rename = "tempPath")]
    pub temp_path: Option<String>,
    #[serde(rename = "magicRollback")]
    pub magic_rollback: Option<bool>,
    #[serde(rename = "sudo")]
    pub sudo: Option<String>,
    #[serde(default, rename = "remoteBuild")]
    pub remote_build: Option<bool>,
    #[serde(rename = "interactiveSudo")]
    pub interactive_sudo: Option<bool>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct NodeSettings {
    pub hostname: String,
    pub profiles: HashMap<String, Profile>,
    #[serde(default, rename = "profilesOrder")]
    pub profiles_order: Vec<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ProfileSettings {
    pub path: String,
    #[serde(rename = "profilePath")]
    pub profile_path: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Profile {
    #[serde(flatten)]
    pub profile_settings: ProfileSettings,
    #[serde(flatten)]
    pub generic_settings: GenericSettings,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Node {
    #[serde(flatten)]
    pub generic_settings: GenericSettings,
    #[serde(flatten)]
    pub node_settings: NodeSettings,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Data {
    #[serde(flatten)]
    pub generic_settings: GenericSettings,
    pub nodes: HashMap<String, Node>,
}

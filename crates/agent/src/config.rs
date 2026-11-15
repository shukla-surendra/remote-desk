//! Persistent agent identity and settings, stored as JSON in the OS config dir
//! (`~/Library/Application Support/remote-desk/agent.json` on macOS).

use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub device_id: Option<String>,
    pub device_token: Option<String>,
    /// Argon2id hash of the unattended-access password, if one was set.
    pub password_hash: Option<String>,
    /// App mode: connect to this control plane instead of the built-in server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_url: Option<String>,
    /// App mode: address the built-in server listens on (default `0.0.0.0:8080`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_bind: Option<String>,
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("remote-desk")
}

pub fn default_path() -> PathBuf {
    config_dir().join("agent.json")
}

pub fn load(path: &Path) -> anyhow::Result<Identity> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Identity::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &Path, id: &Identity) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(id)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
}

/// Ensures only one agent per config file runs; two would fight over the device ID.
/// Keep the returned handle alive for the life of the process.
pub fn lock_single_instance(cfg_path: &Path) -> anyhow::Result<File> {
    if let Some(dir) = cfg_path.parent() {
        fs::create_dir_all(dir)?;
    }
    let lock_path = cfg_path.with_extension("lock");
    let f =
        File::create(&lock_path).with_context(|| format!("creating {}", lock_path.display()))?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(fs::TryLockError::WouldBlock) => bail!("Remote Desk is already running"),
        Err(fs::TryLockError::Error(e)) => Err(e).context("locking agent"),
    }
}

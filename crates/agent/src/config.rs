//! Persistent agent identity, stored as JSON in the OS config dir.

use std::{fs, path::PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub device_id: Option<String>,
    pub device_token: Option<String>,
    /// Argon2id hash of the unattended-access password, if one was set.
    pub password_hash: Option<String>,
}

pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("remote-desk")
        .join("agent.json")
}

pub fn load(path: &PathBuf) -> anyhow::Result<Identity> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Identity::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(path: &PathBuf, id: &Identity) -> anyhow::Result<()> {
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

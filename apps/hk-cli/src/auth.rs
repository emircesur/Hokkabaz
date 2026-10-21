//! API key lookup: env first, then the OS keyring through hk-store secrets.

use anyhow::{Context, Result, bail};
use hk_store::secrets::KeyringSecretStore;
use hk_store::{Secret, SecretStore, Store};

use crate::paths::Paths;
use crate::term;

pub const ENV_VAR: &str = "DEEPSEEK_API_KEY";
const SECRET_NAME: &str = "deepseek";

pub fn deepseek_key() -> Result<String> {
    if let Some(k) = std::env::var(ENV_VAR).ok().filter(|k| !k.trim().is_empty()) {
        return Ok(k.trim().to_owned());
    }
    match KeyringSecretStore::default().get(SECRET_NAME) {
        Ok(Some(s)) => Ok(s.expose().to_owned()),
        Ok(None) => bail!("no DeepSeek API key: set {ENV_VAR} or run `hk auth set`"),
        Err(e) => bail!("no {ENV_VAR} set and keyring unavailable ({e}); export {ENV_VAR}"),
    }
}

/// Read one line from stdin and store it in the keyring.
pub async fn set(paths: &Paths) -> Result<()> {
    term::err("DeepSeek API key (input is visible): ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let key = line.trim();
    if key.is_empty() {
        bail!("empty key");
    }
    std::fs::create_dir_all(&paths.data).context("create data dir")?;
    let store = Store::open(paths.db()).await?;
    store
        .put_secret(&KeyringSecretStore::default(), SECRET_NAME, Secret::new(key))
        .await?;
    term::err("stored in keyring\n");
    Ok(())
}

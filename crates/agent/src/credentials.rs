use cadpad_core::{Error, Result};
use zeroize::Zeroizing;

/// Never Debug/Serialize: keys stay outside tools, project files and event logs.
pub struct Secret(Zeroizing<String>);
impl Secret {
    pub fn new(value: String) -> Result<Self> {
        if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(Error::domain("INVALID_PARAMETER", "Enter a valid API key"));
        }
        Ok(Self(Zeroizing::new(value)))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
pub trait Credentials: Send + Sync {
    fn get(&self) -> Result<Option<Secret>>;
    fn set(&self, secret: Secret) -> Result<()>;
    fn delete(&self) -> Result<()>;
}
pub struct SystemCredentials;
#[cfg(target_os = "macos")]
impl Credentials for SystemCredentials {
    fn get(&self) -> Result<Option<Secret>> {
        let entry =
            keyring::Entry::new("app.cadpad.desktop", "anthropic-api-key").map_err(|_| {
                Error::domain(
                    "CREDENTIAL_UNAVAILABLE",
                    "Could not open the system keychain",
                )
            })?;
        match entry.get_password() {
            Ok(value) => Ok(Some(Secret::new(value)?)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(Error::domain(
                "CREDENTIAL_UNAVAILABLE",
                "Allow Cadpad access to its API key in the system keychain",
            )),
        }
    }
    fn set(&self, secret: Secret) -> Result<()> {
        keyring::Entry::new("app.cadpad.desktop", "anthropic-api-key")
            .and_then(|entry| entry.set_password(secret.expose()))
            .map_err(|_| {
                Error::domain(
                    "CREDENTIAL_UNAVAILABLE",
                    "Could not store the API key in the system keychain",
                )
            })
    }
    fn delete(&self) -> Result<()> {
        match keyring::Entry::new("app.cadpad.desktop", "anthropic-api-key")
            .and_then(|entry| entry.delete_credential())
        {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(Error::domain(
                "CREDENTIAL_UNAVAILABLE",
                "Could not remove the API key from the system keychain",
            )),
        }
    }
}
#[cfg(not(target_os = "macos"))]
impl Credentials for SystemCredentials {
    fn get(&self) -> Result<Option<Secret>> {
        Err(Error::domain(
            "UNSUPPORTED_OPERATION",
            "Native credential storage is currently implemented for macOS",
        ))
    }
    fn set(&self, _: Secret) -> Result<()> {
        self.get().map(|_| ())
    }
    fn delete(&self) -> Result<()> {
        self.get().map(|_| ())
    }
}

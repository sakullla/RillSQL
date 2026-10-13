use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("系统凭据库不可用：{0}")]
    Unavailable(String),
}

pub trait SecretStore: Send + Sync {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

pub struct KeyringStore {
    service: &'static str,
}

impl KeyringStore {
    pub fn new() -> Self {
        Self {
            service: "app.rillsql.desktop",
        }
    }
}

impl Default for KeyringStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeyringStore {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(self.service, account).map_err(|err| SecretError::Unavailable(err.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|err| SecretError::Unavailable(err.to_string()))
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(self.service, account).map_err(|err| SecretError::Unavailable(err.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(SecretError::Unavailable(err.to_string())),
        }
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(self.service, account).map_err(|err| SecretError::Unavailable(err.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(SecretError::Unavailable(err.to_string())),
        }
    }
}

#[derive(Default)]
pub struct MemoryStore {
    values: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    pub fn dump(&self) -> Vec<(String, String)> {
        self.values.lock().expect("secret map").iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }
}

impl SecretStore for MemoryStore {
    fn put(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        self.values
            .lock()
            .expect("secret map")
            .insert(account.to_owned(), secret.to_owned());
        Ok(())
    }

    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        Ok(self.values.lock().expect("secret map").get(account).cloned())
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.values.lock().expect("secret map").remove(account);
        Ok(())
    }
}

#[derive(Default)]
pub struct UnavailableStore;

impl SecretStore for UnavailableStore {
    fn put(&self, _account: &str, _secret: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("Secret Service 未运行".to_owned()))
    }

    fn get(&self, _account: &str) -> Result<Option<String>, SecretError> {
        Err(SecretError::Unavailable("Secret Service 未运行".to_owned()))
    }

    fn delete(&self, _account: &str) -> Result<(), SecretError> {
        Ok(())
    }
}

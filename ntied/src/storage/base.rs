use std::path::{Path, PathBuf};

use anyhow::anyhow;
use argon2::{Algorithm, Argon2, Params, Version};
use rand::RngCore as _;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use tokio_sqlite::{Connection, Value};

use crate::models::Base64;

pub struct Storage {
    path: PathBuf,
    connection: Connection,
    password_hash: Vec<u8>,
}

impl Storage {
    pub async fn open(path: &Path, password: &str) -> Result<Self, anyhow::Error> {
        Self::validate_password(&password)?;
        let path = path.to_owned();
        let meta_path = path.join("meta.json");
        let data_path = path.join("data.db");
        let password = password.to_owned();
        let meta = Self::load_meta(&meta_path).await?;
        let key = Self::get_key(&meta, &password)?;
        let mut connection = Connection::open(data_path).await?;
        Self::key_connection(&mut connection, &key).await?;
        let password_hash = sha2::Sha256::digest(&password).to_vec();
        Ok(Self {
            path,
            connection,
            password_hash,
        })
    }

    pub async fn create(path: &Path, password: &str) -> Result<Self, anyhow::Error> {
        Self::validate_password(&password)?;
        let path = path.to_owned();
        let meta_path = path.join("meta.json");
        let data_path = path.join("data.db");
        let password = password.to_owned();
        let mut salt = vec![0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let meta = Meta {
            hash: Hash::Argon2id {
                m_cost: 64 * 1024,
                t_cost: 3,
                p_cost: 2,
            },
            salt: Base64(salt),
        };
        let key = Self::get_key(&meta, &password)?;
        drop(tokio::fs::remove_file(&data_path).await);
        let mut connection = Connection::open(&data_path).await?;
        Self::key_connection(&mut connection, &key).await?;
        Self::write_meta(&meta_path, &meta).await?;
        let password_hash = sha2::Sha256::digest(&password).to_vec();
        Ok(Self {
            path,
            connection,
            password_hash,
        })
    }

    pub async fn change_password(
        &mut self,
        password: &str,
        new_password: &str,
    ) -> Result<(), anyhow::Error> {
        Self::validate_password(&password)?;
        Self::validate_password(&new_password)?;
        let password_hash = sha2::Sha256::digest(&password).to_vec();
        if password_hash != self.password_hash {
            return Err(anyhow!("Incorrect password"));
        }
        let mut salt = vec![0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let meta = Meta {
            hash: Hash::Argon2id {
                m_cost: 64 * 1024,
                t_cost: 3,
                p_cost: 2,
            },
            salt: Base64(salt),
        };
        let key = Self::get_key(&meta, &new_password)?;
        let meta_path = self.path.join("meta.json");
        Self::write_meta(&meta_path, &meta).await?;
        self.rekey_connection(&key).await?;
        self.password_hash = sha2::Sha256::digest(&new_password).to_vec();
        Ok(())
    }

    pub async fn connection(&mut self) -> &mut Connection {
        &mut self.connection
    }

    async fn load_meta(path: &Path) -> Result<Meta, anyhow::Error> {
        let data = tokio::fs::read_to_string(path).await?;
        let meta = serde_json::from_str(&data)?;
        Ok(meta)
    }

    async fn write_meta(path: &Path, meta: &Meta) -> Result<(), anyhow::Error> {
        let data = serde_json::to_string_pretty(meta)?;
        tokio::fs::write(path, data).await?;
        Ok(())
    }

    async fn key_connection(connection: &mut Connection, key: &[u8]) -> Result<(), anyhow::Error> {
        let hex_key = hex::encode(key).to_uppercase();
        let pragma_key = format!("PRAGMA key = \"x'{}'\"", hex_key);
        connection.query(pragma_key, Vec::<Value>::new()).await?;
        Ok(())
    }

    async fn rekey_connection(&mut self, key: &[u8]) -> Result<(), anyhow::Error> {
        let hex_key = hex::encode(key).to_uppercase();
        let pragma_key = format!("PRAGMA rekey = \"x'{}'\"", hex_key);
        self.connection
            .query(pragma_key, Vec::<Value>::new())
            .await?;
        Ok(())
    }

    fn get_key(meta: &Meta, password: &str) -> Result<Vec<u8>, anyhow::Error> {
        match meta.hash {
            Hash::Argon2id {
                m_cost,
                t_cost,
                p_cost,
            } => {
                let params = Params::new(m_cost, t_cost, p_cost, Some(32))
                    .map_err(|err| anyhow!("Incorrect Argon2id params: {err}"))?;
                let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
                let mut hash = [0u8; 32];
                argon
                    .hash_password_into(password.as_bytes(), &meta.salt.0, &mut hash)
                    .map_err(|err| anyhow!("Failed to hash password with Argon2id: {err}"))?;
                Ok(hash.to_vec())
            }
        }
    }

    fn validate_password(password: &str) -> Result<(), anyhow::Error> {
        if password.len() < 4 {
            return Err(anyhow!("Password is too short"));
        }
        if password.len() > 64 {
            return Err(anyhow!("Password is too long"));
        }
        return Ok(());
    }
}

#[derive(Serialize, Deserialize)]
struct Meta {
    hash: Hash,
    salt: Base64,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type")]
enum Hash {
    Argon2id {
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    },
}

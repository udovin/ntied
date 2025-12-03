use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE;
use ntied_crypto::{Error, PublicKey};
use sha2::{Digest as _, Sha256};

// TODO: Deprecate this.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Address([u8; Self::LEN]);

impl Address {
    pub const LEN: usize = 33;

    const _CHECK: () = {
        assert!(Self::LEN % 3 == 0);
    };

    pub fn from_bytes(hash: [u8; Self::LEN]) -> Self {
        Self(hash)
    }

    pub fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl From<Address> for [u8; Address::LEN] {
    fn from(address: Address) -> Self {
        address.0
    }
}

impl TryFrom<&[u8]> for Address {
    type Error = Error;

    fn try_from(hash: &[u8]) -> Result<Self, Self::Error> {
        Ok(Self(hash.try_into()?))
    }
}

impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&URL_SAFE.encode(self.as_bytes()))
    }
}

impl std::str::FromStr for Address {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = URL_SAFE
            .decode(s)?
            .try_into()
            .map_err(|v: Vec<u8>| format!("Invalid address length: {}", v.len()))?;
        Ok(Self::from_bytes(bytes))
    }
}

impl std::fmt::Debug for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self}")
    }
}

pub trait ToAddress {
    fn to_address(&self) -> Result<Address, Error>;
}

impl ToAddress for Address {
    fn to_address(&self) -> Result<Address, Error> {
        Ok(*self)
    }
}

impl ToAddress for PublicKey {
    fn to_address(&self) -> Result<Address, Error> {
        let mut hasher = Sha256::new();
        hasher.update(self.to_bytes()?);
        let hash = hasher.finalize();
        let mut bytes = [0u8; Address::LEN];
        bytes[0] = 1;
        bytes[1..].copy_from_slice(hash.as_slice());
        Ok(Address::from_bytes(bytes))
    }
}

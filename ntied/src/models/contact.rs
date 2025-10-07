use lazy_static::lazy_static;
use ntied_crypto::PublicKey;
use ntied_transport::Address;
use tokio_sqlite::Value;

use super::{
    ColumnIndex, DateTime, value_as_address, value_as_bytes, value_as_datetime, value_as_i64,
    value_as_string, value_as_string_opt,
};

#[derive(Clone)]
pub struct Contact {
    pub id: i64,
    // Network address of the remote contact.
    pub address: Address,
    // Public key of the remote contact.
    pub public_key: PublicKey,
    // Local name that overrides name in UI.
    pub local_name: Option<String>,
    // Name obtained from the remote contact.
    pub name: String,
    pub create_time: DateTime,
}

impl Contact {
    pub fn columns() -> &'static ColumnIndex {
        lazy_static! {
            static ref COLUMNS: ColumnIndex = ColumnIndex::builder()
                .add("id")
                .add("address")
                .add("public_key")
                .add("local_name")
                .add("name")
                .add("create_time")
                .build();
        }
        &COLUMNS
    }

    pub fn values(&self, columns: &ColumnIndex) -> Vec<Value> {
        let mut values = columns.new_values();
        columns.set_value(&mut values, "id", self.id);
        columns.set_value(&mut values, "address", self.address.to_string());
        columns.set_value(
            &mut values,
            "public_key",
            self.public_key.to_bytes().unwrap(),
        );
        columns.set_value(&mut values, "local_name", self.local_name.clone());
        columns.set_value(&mut values, "name", self.name.clone());
        columns.set_value(
            &mut values,
            "create_time",
            self.create_time.0.timestamp_micros(),
        );
        values
    }

    pub fn from_values(values: Vec<Value>, columns: &ColumnIndex) -> Result<Self, anyhow::Error> {
        Ok(Self {
            id: value_as_i64(columns.get_value(&values, "id").unwrap())?,
            address: value_as_address(columns.get_value(&values, "address").unwrap())?,
            public_key: PublicKey::from_bytes(&value_as_bytes(
                columns.get_value(&values, "public_key").unwrap(),
            )?)
            .map_err(anyhow::Error::msg)?,
            local_name: value_as_string_opt(columns.get_value(&values, "local_name").unwrap())?,
            name: value_as_string(columns.get_value(&values, "name").unwrap())?,
            create_time: value_as_datetime(columns.get_value(&values, "create_time").unwrap())?,
        })
    }
}

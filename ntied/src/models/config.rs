use lazy_static::lazy_static;
use tokio_sqlite::Value;

use super::{ColumnIndex, value_as_i64, value_as_string};

#[derive(Debug, Clone)]
pub struct Config {
    pub id: i64,
    pub key: String,
    pub value: serde_json::Value,
}

impl Config {
    pub fn columns() -> &'static ColumnIndex {
        lazy_static! {
            static ref COLUMNS: ColumnIndex = ColumnIndex::builder()
                .add("id")
                .add("key")
                .add("value")
                .build();
        }
        &COLUMNS
    }

    pub fn values(&self, columns: &ColumnIndex) -> Vec<Value> {
        let mut values = columns.new_values();
        columns.set_value(&mut values, "id", self.id);
        columns.set_value(&mut values, "key", self.key.clone());
        columns.set_value(&mut values, "value", self.value.to_string());
        values
    }

    pub fn from_values(values: Vec<Value>, columns: &ColumnIndex) -> Result<Self, anyhow::Error> {
        Ok(Self {
            id: value_as_i64(columns.get_value(&values, "id").unwrap())?,
            key: value_as_string(columns.get_value(&values, "key").unwrap())?,
            value: serde_json::from_str(&value_as_string(
                columns.get_value(&values, "value").unwrap(),
            )?)?,
        })
    }
}

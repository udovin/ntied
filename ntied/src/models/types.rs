use std::collections::{HashMap, hash_map};
use std::fmt::Debug;
use std::str::FromStr;

use anyhow::anyhow;
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use ntied_transport::Address;
use serde::{Deserialize, Serialize};
use tokio_sqlite::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DateTime(pub chrono::DateTime<chrono::Utc>);

impl DateTime {
    pub fn now() -> Self {
        Self(chrono::Utc::now())
    }
}

impl Serialize for DateTime {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let micros = self.0.timestamp_micros();
        serializer.serialize_i64(micros)
    }
}

impl<'de> Deserialize<'de> for DateTime {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let micros = i64::deserialize(deserializer)?;
        Ok(Self(
            chrono::DateTime::<chrono::Utc>::from_timestamp_micros(micros)
                .ok_or(Error::custom("cannot deserialize DateTime from micros"))?,
        ))
    }
}

#[derive(Clone, Debug)]
pub struct Base64(pub Vec<u8>);

impl Base64 {
    pub fn new(data: Vec<u8>) -> Self {
        Self(data)
    }
}

impl Serialize for Base64 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let encoded = BASE64_STANDARD.encode(&self.0);
        serializer.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for Base64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let encoded = String::deserialize(deserializer)?;
        let data = BASE64_STANDARD.decode(encoded).map_err(Error::custom)?;
        Ok(Self(data))
    }
}

#[derive(Clone)]
pub struct ColumnIndex {
    column_map: HashMap<String, usize>,
}

impl ColumnIndex {
    pub fn builder() -> ColumnIndexBuilder {
        ColumnIndexBuilder {
            column_map: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.column_map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.column_map.is_empty()
    }

    pub fn get(&self, name: impl AsRef<str>) -> Option<usize> {
        self.column_map.get(name.as_ref()).cloned()
    }

    pub fn columns(&self) -> Vec<String> {
        let mut columns = vec![String::new(); self.column_map.len()];
        for (name, index) in self.column_map.iter() {
            columns[*index] = name.clone();
        }
        columns
    }

    pub fn get_value<'v>(&self, values: &'v [Value], name: impl AsRef<str>) -> Option<&'v Value> {
        assert_eq!(
            self.len(),
            values.len(),
            "Columns length differs from values length"
        );
        self.get(name).and_then(|index| values.get(index))
    }

    pub fn set_value(
        &self,
        values: &mut [Value],
        name: impl AsRef<str>,
        value: impl Into<Value>,
    ) -> bool {
        assert_eq!(
            self.len(),
            values.len(),
            "Columns length differs from values length"
        );
        match self.get(name) {
            Some(index) => values
                .get_mut(index)
                .map(|v| {
                    *v = value.into();
                    true
                })
                .unwrap_or(false),
            None => false,
        }
    }

    pub fn new_values(&self) -> Vec<Value> {
        vec![Value::Null; self.len()]
    }
}

impl Debug for ColumnIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ColumnIndex").field(&self.columns()).finish()
    }
}

pub struct ColumnIndexBuilder {
    column_map: HashMap<String, usize>,
}

impl ColumnIndexBuilder {
    pub fn add(&mut self, name: impl Into<String>) -> &mut Self {
        let name = name.into();
        let index = self.column_map.len();
        if let hash_map::Entry::Vacant(entry) = self.column_map.entry(name) {
            entry.insert(index);
        }
        self
    }

    pub fn build(&mut self) -> ColumnIndex {
        let column_map = std::mem::take(&mut self.column_map);
        ColumnIndex { column_map }
    }
}

pub(super) fn value_as_i64(v: &Value) -> Result<i64, anyhow::Error> {
    match v {
        Value::Integer(i) => Ok(*i),
        _ => Err(anyhow!("Expected Integer for i64")),
    }
}

pub(super) fn value_as_u64(v: &Value) -> Result<u64, anyhow::Error> {
    match v {
        Value::Integer(i) if *i >= 0 => Ok(*i as u64),
        _ => Err(anyhow!("Expected non-negative Integer for u64")),
    }
}

pub(super) fn value_as_bool(v: &Value) -> Result<bool, anyhow::Error> {
    match v {
        Value::Integer(i) => Ok(*i != 0),
        _ => Err(anyhow!("Expected Integer for bool")),
    }
}

pub(super) fn value_as_string(v: &Value) -> Result<String, anyhow::Error> {
    match v {
        Value::Text(s) => Ok(s.clone()),
        _ => Err(anyhow!("Expected Text for String")),
    }
}

pub(super) fn value_as_address(v: &Value) -> Result<Address, anyhow::Error> {
    match v {
        Value::Text(s) => {
            let addr = Address::from_str(s).map_err(|e| anyhow!("Invalid address: {}", e))?;
            Ok(addr)
        }
        _ => Err(anyhow!("Expected Text for Address")),
    }
}

pub(super) fn value_as_uuid(v: &Value) -> Result<Uuid, anyhow::Error> {
    match v {
        Value::Text(s) => {
            let addr = Uuid::from_str(s).map_err(|e| anyhow!("Invalid Uuid: {}", e))?;
            Ok(addr)
        }
        _ => Err(anyhow!("Expected Text for Uuid")),
    }
}

pub(super) fn value_as_bytes(v: &Value) -> Result<Vec<u8>, anyhow::Error> {
    match v {
        Value::Blob(b) => Ok(b.clone()),
        _ => Err(anyhow!("Expected Blob for Vec<u8>")),
    }
}

pub(super) fn value_as_datetime(v: &Value) -> Result<DateTime, anyhow::Error> {
    match v {
        Value::Integer(i) => {
            let datetime = chrono::DateTime::<chrono::Utc>::from_timestamp_micros(*i)
                .ok_or(anyhow!("Cannot convert Integer to DateTime"))?;
            Ok(DateTime(datetime))
        }
        _ => Err(anyhow!("Expected Integer for DateTime")),
    }
}

pub(super) fn value_as_u64_opt(v: &Value) -> Result<Option<u64>, anyhow::Error> {
    match v {
        Value::Null => Ok(None),
        v => value_as_u64(v).map(|v| Some(v)),
    }
}

pub(super) fn value_as_string_opt(v: &Value) -> Result<Option<String>, anyhow::Error> {
    match v {
        Value::Null => Ok(None),
        v => value_as_string(v).map(|v| Some(v)),
    }
}

pub(super) fn value_as_datetime_opt(v: &Value) -> Result<Option<DateTime>, anyhow::Error> {
    match v {
        Value::Null => Ok(None),
        v => value_as_datetime(v).map(|v| Some(v)),
    }
}

use anyhow::anyhow;
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tokio_sqlite::Value;
use uuid::Uuid;

use super::{
    ColumnIndex, DateTime, value_as_bool, value_as_datetime, value_as_datetime_opt, value_as_i64,
    value_as_string, value_as_u64_opt, value_as_uuid,
};

#[derive(Debug, Clone)]
pub struct Message {
    pub id: i64,
    pub contact_id: i64,
    pub message_id: Uuid,
    pub log_id: Option<u64>,
    pub incoming: bool,
    pub kind: MessageKind,
    pub create_time: DateTime,
    pub receive_time: Option<DateTime>,
    pub read_time: Option<DateTime>,
}

impl Message {
    pub fn columns() -> &'static ColumnIndex {
        lazy_static! {
            static ref COLUMNS: ColumnIndex = ColumnIndex::builder()
                .add("id")
                .add("contact_id")
                .add("message_id")
                .add("log_id")
                .add("incoming")
                .add("kind")
                .add("content")
                .add("create_time")
                .add("receive_time")
                .add("read_time")
                .build();
        }
        &COLUMNS
    }

    pub fn values(&self, columns: &ColumnIndex) -> Vec<Value> {
        let mut values = columns.new_values();
        columns.set_value(&mut values, "id", self.id);
        columns.set_value(&mut values, "contact_id", self.contact_id);
        columns.set_value(&mut values, "message_id", self.message_id.to_string());
        columns.set_value(&mut values, "log_id", self.log_id.map(|v| v as i64));
        columns.set_value(&mut values, "incoming", self.incoming);
        columns.set_value(&mut values, "kind", self.kind.name().to_string());
        columns.set_value(&mut values, "content", self.kind.content());
        columns.set_value(
            &mut values,
            "create_time",
            self.create_time.0.timestamp_micros(),
        );
        columns.set_value(
            &mut values,
            "receive_time",
            self.receive_time.map(|v| v.0.timestamp_micros()),
        );
        columns.set_value(
            &mut values,
            "read_time",
            self.read_time.map(|v| v.0.timestamp_micros()),
        );
        values
    }

    pub fn from_values(values: Vec<Value>, columns: &ColumnIndex) -> Result<Self, anyhow::Error> {
        let kind = value_as_string(columns.get_value(&values, "kind").unwrap())?;
        let content = value_as_string(columns.get_value(&values, "content").unwrap())?;
        let message_kind = MessageKind::parse(kind, content)?;
        Ok(Self {
            id: value_as_i64(columns.get_value(&values, "id").unwrap())?,
            contact_id: value_as_i64(columns.get_value(&values, "contact_id").unwrap())?,
            message_id: value_as_uuid(columns.get_value(&values, "message_id").unwrap())?,
            log_id: value_as_u64_opt(columns.get_value(&values, "log_id").unwrap())?,
            incoming: value_as_bool(columns.get_value(&values, "incoming").unwrap())?,
            kind: message_kind,
            create_time: value_as_datetime(columns.get_value(&values, "create_time").unwrap())?,
            receive_time: value_as_datetime_opt(
                columns.get_value(&values, "receive_time").unwrap(),
            )?,
            read_time: value_as_datetime_opt(columns.get_value(&values, "read_time").unwrap())?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageKind {
    Text(String),
}

impl MessageKind {
    pub fn name(&self) -> &str {
        match self {
            Self::Text(_) => "text",
        }
    }

    pub fn content(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
        }
    }

    pub fn parse(kind: String, content: String) -> Result<Self, anyhow::Error> {
        match kind.as_str() {
            "text" => Ok(Self::Text(content)),
            _ => Err(anyhow!("Unknown message kind: {}", kind)),
        }
    }
}

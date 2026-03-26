use lazy_static::lazy_static;
use ntied_transport::{PEER_ID_SIZE, PeerId};
use tokio_sqlite::Value;

use super::{
    ColumnIndex, DateTime, value_as_bytes, value_as_datetime, value_as_i64, value_as_string,
    value_as_string_opt,
};

#[derive(Clone)]
pub struct Contact {
    pub id: i64,
    // Peer ID of the remote contact.
    pub peer_id: PeerId,
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
                .add("peer_id")
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
        columns.set_value(&mut values, "peer_id", self.peer_id.to_bytes().to_vec());
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
        let peer_id_bytes = value_as_bytes(columns.get_value(&values, "peer_id").unwrap())?;
        let peer_id_arr: [u8; PEER_ID_SIZE] = peer_id_bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid peer_id length"))?;
        Ok(Self {
            id: value_as_i64(columns.get_value(&values, "id").unwrap())?,
            peer_id: PeerId::from_bytes(peer_id_arr),
            local_name: value_as_string_opt(columns.get_value(&values, "local_name").unwrap())?,
            name: value_as_string(columns.get_value(&values, "name").unwrap())?,
            create_time: value_as_datetime(columns.get_value(&values, "create_time").unwrap())?,
        })
    }
}

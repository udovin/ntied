use ntied::models::{Config, Contact, DateTime, Message, MessageKind};
use ntied_crypto::PrivateKey;
use ntied_transport::ToAddress;
use tokio_sqlite::Value;
use uuid::Uuid;

#[test]
fn test_contact_values_roundtrip() {
    // Arrange
    let key = PrivateKey::generate().expect("failed to generate key");
    let public_key = key.public_key().clone();
    let address = public_key.to_address().expect("to_address failed");
    let contact = Contact {
        id: 123,
        address,
        public_key: public_key.clone(),
        local_name: Some("Local Name".to_string()),
        name: "Remote Name".to_string(),
        create_time: DateTime::now(),
    };
    let columns = Contact::columns();
    // Act: serialize to row values
    let values = contact.values(columns);
    // Assert serialization (check types and contents by columns)
    match columns.get_value(&values, "id").unwrap() {
        Value::Integer(i) => assert_eq!(*i, 123),
        v => panic!("id should be Integer, got {:?}", v),
    }
    match columns.get_value(&values, "address").unwrap() {
        Value::Text(s) => assert_eq!(s, &contact.address.to_string()),
        v => panic!("address should be Text, got {:?}", v),
    }
    match columns.get_value(&values, "public_key").unwrap() {
        Value::Blob(b) => assert_eq!(b, &public_key.to_bytes().unwrap()),
        v => panic!("public_key should be Blob, got {:?}", v),
    }
    match columns.get_value(&values, "local_name").unwrap() {
        Value::Text(s) => assert_eq!(s, "Local Name"),
        v => panic!("local_name should be Text, got {:?}", v),
    }
    match columns.get_value(&values, "name").unwrap() {
        Value::Text(s) => assert_eq!(s, "Remote Name"),

        v => panic!("name should be Text, got {:?}", v),
    }
    match columns.get_value(&values, "create_time").unwrap() {
        Value::Integer(micros) => assert_eq!(*micros, contact.create_time.0.timestamp_micros()),
        v => panic!("create_time should be Integer, got {:?}", v),
    }
    // Act: deserialize back from row values
    let decoded =
        Contact::from_values(values.clone(), columns).expect("Contact::from_values failed");
    // Assert deserialization matches original data
    assert_eq!(decoded.id, contact.id);
    assert_eq!(decoded.address, contact.address);
    assert_eq!(
        decoded.public_key.to_bytes().unwrap(),
        contact.public_key.to_bytes().unwrap()
    );
    assert_eq!(decoded.local_name, contact.local_name);
    assert_eq!(decoded.name, contact.name);
    assert_eq!(
        decoded.create_time.0.timestamp_micros(),
        contact.create_time.0.timestamp_micros()
    );
}

#[test]
fn test_contact_values_roundtrip_with_nones() {
    // Arrange
    let key = PrivateKey::generate().expect("failed to generate key");
    let public_key = key.public_key().clone();
    let address = public_key.to_address().expect("to_address failed");
    let contact = Contact {
        id: 1,
        address,
        public_key: public_key.clone(),
        local_name: None,
        name: "1".into(),
        create_time: DateTime::now(),
    };
    let columns = Contact::columns();
    // Act
    let values = contact.values(columns);
    // Ensure that optional fields are represented as Null
    match columns.get_value(&values, "local_name").unwrap() {
        Value::Null => {}
        v => panic!("local_name should be Null, got {:?}", v),
    }
    match columns.get_value(&values, "name").unwrap() {
        Value::Text(_) => {}
        v => panic!("name should be Null, got {:?}", v),
    }
    // Roundtrip
    let decoded = Contact::from_values(values, columns).expect("Contact::from_values failed");
    assert_eq!(decoded.id, contact.id);
    assert_eq!(decoded.address, contact.address);
    assert_eq!(
        decoded.public_key.to_bytes().unwrap(),
        contact.public_key.to_bytes().unwrap()
    );
    assert_eq!(decoded.local_name, None);
    assert_eq!(decoded.name, "1");
}

#[test]
fn test_config_values_roundtrip() {
    // Arrange
    let cfg = Config {
        id: 7,
        key: "ui.theme".to_string(),
        value: serde_json::json!({"a": 1, "b": true}),
    };
    let columns = Config::columns();
    // Act
    let values = cfg.values(columns);
    // Assert: verify produced sqlite Values
    match columns.get_value(&values, "id").unwrap() {
        Value::Integer(i) => assert_eq!(*i, 7),
        v => panic!("id should be Integer, got {:?}", v),
    }
    match columns.get_value(&values, "key").unwrap() {
        Value::Text(s) => assert_eq!(s, "ui.theme"),
        v => panic!("key should be Text, got {:?}", v),
    }
    // serde_json::Value::to_string produces canonical minified JSON
    match columns.get_value(&values, "value").unwrap() {
        Value::Text(s) => assert!(s == r#"{"a":1,"b":true}"# || s == r#"{"b":true,"a":1}"#),
        v => panic!("value should be Text(JSON), got {:?}", v),
    }
    // Roundtrip
    let decoded = Config::from_values(values, columns).expect("Config::from_values failed");
    assert_eq!(decoded.id, cfg.id);
    assert_eq!(decoded.key, cfg.key);
    assert_eq!(decoded.value, cfg.value);
}

#[test]
fn test_message_values_serialization() {
    // Arrange
    let msg = Message {
        id: 42,
        contact_id: 314,
        message_id: Uuid::now_v7(),
        log_id: Some(999),
        incoming: true,
        kind: MessageKind::Text("hello world".to_string()),
        create_time: DateTime::now(),
        receive_time: Some(DateTime::now()),
        read_time: None,
    };
    let columns = Message::columns();
    // Act
    let values = msg.values(columns);
    // Assert serialization layout and conversions
    match columns.get_value(&values, "id").unwrap() {
        Value::Integer(i) => assert_eq!(*i, 42),
        v => panic!("id should be Integer, got {:?}", v),
    }
    match columns.get_value(&values, "contact_id").unwrap() {
        Value::Integer(i) => assert_eq!(*i, 314),
        v => panic!("contact_id should be Integer, got {:?}", v),
    }
    match columns.get_value(&values, "message_id").unwrap() {
        Value::Text(s) => assert_eq!(s, &msg.message_id.to_string()),
        v => panic!("message_id should be Text, got {:?}", v),
    }
    match columns.get_value(&values, "log_id").unwrap() {
        Value::Integer(i) => assert_eq!(*i, 999),
        v => panic!("log_id should be Integer, got {:?}", v),
    }
    match columns.get_value(&values, "incoming").unwrap() {
        Value::Integer(i) => assert_eq!(*i, 1), // true -> 1
        v => panic!("incoming should be stored as Integer, got {:?}", v),
    }
    match columns.get_value(&values, "kind").unwrap() {
        Value::Text(s) => assert_eq!(s, "text"),
        v => panic!("kind should be Text, got {:?}", v),
    }
    match columns.get_value(&values, "content").unwrap() {
        Value::Text(s) => assert_eq!(s, "hello world"),
        v => panic!("content should be Text, got {:?}", v),
    }
    match columns.get_value(&values, "create_time").unwrap() {
        Value::Integer(m) => assert_eq!(*m, msg.create_time.0.timestamp_micros()),
        v => panic!("create_time should be Integer(micros), got {:?}", v),
    }
    match columns.get_value(&values, "receive_time").unwrap() {
        Value::Integer(m) => assert_eq!(*m, msg.receive_time.unwrap().0.timestamp_micros()),
        v => panic!("receive_time should be Integer(micros), got {:?}", v),
    }
    match columns.get_value(&values, "read_time").unwrap() {
        Value::Null => {}
        v => panic!("read_time should be Null, got {:?}", v),
    }
}

#[test]
fn test_message_values_roundtrip_variants() {
    // Variant 1: receive_time = Some, read_time = Some
    let msg1 = Message {
        id: 1,
        contact_id: 2,
        message_id: Uuid::now_v7(),
        log_id: Some(55),
        incoming: false,
        kind: MessageKind::Text("payload-1".to_string()),
        create_time: DateTime::now(),
        receive_time: Some(DateTime::now()),
        read_time: Some(DateTime::now()),
    };
    let columns = Message::columns();
    let values1 = msg1.values(columns);
    let decoded1 =
        Message::from_values(values1, columns).expect("Message::from_values failed (v1)");
    assert_eq!(decoded1.id, msg1.id);
    assert_eq!(decoded1.contact_id, msg1.contact_id);
    assert_eq!(decoded1.message_id, msg1.message_id);
    assert_eq!(decoded1.log_id, msg1.log_id);
    assert_eq!(decoded1.incoming, msg1.incoming);
    match decoded1.kind {
        MessageKind::Text(s) => assert_eq!(s, "payload-1"),
    }
    assert_eq!(
        decoded1.create_time.0.timestamp_micros(),
        msg1.create_time.0.timestamp_micros()
    );
    assert_eq!(
        decoded1.receive_time.map(|d| d.0.timestamp_micros()),
        msg1.receive_time.map(|d| d.0.timestamp_micros())
    );
    assert_eq!(
        decoded1.read_time.map(|d| d.0.timestamp_micros()),
        msg1.read_time.map(|d| d.0.timestamp_micros())
    );
    // Variant 2: receive_time = None, read_time = None
    let msg2 = Message {
        id: 10,
        contact_id: 20,
        message_id: Uuid::now_v7(),
        log_id: None,
        incoming: true,
        kind: MessageKind::Text("payload-2".to_string()),
        create_time: DateTime::now(),
        receive_time: None,
        read_time: None,
    };
    let values2 = msg2.values(columns);
    let decoded2 =
        Message::from_values(values2, columns).expect("Message::from_values failed (v2)");
    assert_eq!(decoded2.id, msg2.id);
    assert_eq!(decoded2.contact_id, msg2.contact_id);
    assert_eq!(decoded2.message_id, msg2.message_id);
    assert_eq!(decoded2.log_id, msg2.log_id);
    assert_eq!(decoded2.incoming, msg2.incoming);
    match decoded2.kind {
        MessageKind::Text(s) => assert_eq!(s, "payload-2"),
    }
    assert_eq!(
        decoded2.create_time.0.timestamp_micros(),
        msg2.create_time.0.timestamp_micros()
    );
    assert!(decoded2.receive_time.is_none());
    assert!(decoded2.read_time.is_none());
}

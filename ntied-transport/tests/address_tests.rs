use std::str::FromStr as _;

use base64::Engine;
use ntied_crypto::PrivateKey;
use ntied_transport::{Address, ToAddress};

/// Test creating Address from bytes and converting back
#[test]
fn test_address_from_bytes() {
    let mut bytes = [0u8; 33];
    for i in 0..33 {
        bytes[i] = i as u8;
    }

    let address = Address::from_bytes(bytes);
    assert_eq!(address.as_bytes(), &bytes);
}

/// Test Address to string conversion
#[test]
fn test_address_to_string() {
    let bytes = [42u8; 33];
    let address = Address::from_bytes(bytes);

    let address_string = address.to_string();
    assert!(!address_string.is_empty());
    // Base64 URL-safe encoding of 30 bytes should produce 40 characters
    assert_eq!(address_string.len(), 44);
}

/// Test Address from string conversion
#[test]
fn test_address_from_string() {
    let original_bytes = [123u8; 33];
    let original_address = Address::from_bytes(original_bytes);
    let address_string = original_address.to_string();

    let parsed_address = Address::from_str(&address_string).unwrap();
    assert_eq!(parsed_address.as_bytes(), original_address.as_bytes());
}

/// Test roundtrip conversion: bytes -> Address -> string -> Address -> bytes
#[test]
fn test_address_roundtrip() {
    let mut bytes = [0u8; 33];
    for i in 0..33 {
        bytes[i] = (i * 7) as u8;
    }

    let address1 = Address::from_bytes(bytes);
    let string_repr = address1.to_string();
    let address2 = Address::from_str(&string_repr).unwrap();

    assert_eq!(address1, address2);
    assert_eq!(address1.as_bytes(), address2.as_bytes());
}

/// Test Address equality
#[test]
fn test_address_equality() {
    let bytes1 = [1u8; 33];
    let bytes2 = [1u8; 33];
    let bytes3 = [2u8; 33];

    let address1 = Address::from_bytes(bytes1);
    let address2 = Address::from_bytes(bytes2);
    let address3 = Address::from_bytes(bytes3);

    assert_eq!(address1, address2);
    assert_ne!(address1, address3);
}

/// Test Address copy trait
#[test]
fn test_address_copy() {
    let bytes = [99u8; 33];
    let address1 = Address::from_bytes(bytes);
    let address2 = address1; // Copy

    assert_eq!(address1, address2);
    assert_eq!(address1.as_bytes(), address2.as_bytes());
}

/// Test Address clone trait
#[test]
fn test_address_clone() {
    let bytes = [88u8; 33];
    let address1 = Address::from_bytes(bytes);
    let address2 = address1.clone();

    assert_eq!(address1, address2);
    assert_eq!(address1.as_bytes(), address2.as_bytes());
}

/// Test ToAddress trait for Address itself
#[test]
fn test_to_address_for_address() {
    let bytes = [77u8; 33];
    let address = Address::from_bytes(bytes);

    let converted = address.to_address().unwrap();
    assert_eq!(converted, address);
}

/// Test ToAddress trait for PublicKey
#[test]
fn test_to_address_for_public_key() {
    let private_key = PrivateKey::generate().unwrap();
    let public_key = private_key.public_key();

    let address = public_key.to_address().unwrap();
    assert_eq!(address.as_bytes().len(), 33);

    // Same public key should produce same address
    let address2 = public_key.to_address().unwrap();
    assert_eq!(address, address2);
}

/// Test that different public keys produce different addresses
#[test]
fn test_different_keys_different_addresses() {
    let private_key1 = PrivateKey::generate().unwrap();
    let public_key1 = private_key1.public_key();

    let private_key2 = PrivateKey::generate().unwrap();
    let public_key2 = private_key2.public_key();

    let address1 = public_key1.to_address().unwrap();
    let address2 = public_key2.to_address().unwrap();

    assert_ne!(address1, address2);
}

/// Test Address from invalid base64 string
#[test]
fn test_address_from_invalid_base64() {
    let invalid_base64 = "!!!invalid base64!!!";
    let result = Address::from_str(invalid_base64);
    assert!(result.is_err());
}

/// Test Address from string with wrong length
#[test]
fn test_address_from_wrong_length_string() {
    // Create a valid base64 string but with wrong decoded length
    let wrong_length_bytes = [1u8; 20]; // Should be 30 bytes
    let wrong_length_string = base64::engine::general_purpose::URL_SAFE.encode(&wrong_length_bytes);

    let result = Address::from_str(&wrong_length_string);
    assert!(result.is_err());

    // Test with too long data
    let too_long_bytes = [2u8; 40]; // Should be 30 bytes
    let too_long_string = base64::engine::general_purpose::URL_SAFE.encode(&too_long_bytes);

    let result = Address::from_str(&too_long_string);
    assert!(result.is_err());
}

/// Test Address from empty string
#[test]
fn test_address_from_empty_string() {
    let result = Address::from_str("");
    assert!(result.is_err());
}

/// Test Address hash consistency
#[test]
fn test_address_hash_consistency() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let bytes = [55u8; 33];
    let address1 = Address::from_bytes(bytes);
    let address2 = Address::from_bytes(bytes);

    let mut hasher1 = DefaultHasher::new();
    address1.hash(&mut hasher1);
    let hash1 = hasher1.finish();

    let mut hasher2 = DefaultHasher::new();
    address2.hash(&mut hasher2);
    let hash2 = hasher2.finish();

    assert_eq!(hash1, hash2);
}

/// Test Address can be used as HashMap key
#[test]
fn test_address_as_hashmap_key() {
    use std::collections::HashMap;

    let mut map = HashMap::new();

    let address1 = Address::from_bytes([10u8; 33]);
    let address2 = Address::from_bytes([20u8; 33]);
    let address3 = Address::from_bytes([10u8; 33]); // Same as address1

    map.insert(address1, "value1");
    map.insert(address2, "value2");

    assert_eq!(map.get(&address1), Some(&"value1"));
    assert_eq!(map.get(&address2), Some(&"value2"));
    assert_eq!(map.get(&address3), Some(&"value1")); // Should find address1's value
    assert_eq!(map.len(), 2);
}

/// Test specific Address patterns
#[test]
fn test_address_patterns() {
    // All zeros
    let zeros = [0u8; 33];
    let zero_address = Address::from_bytes(zeros);
    let zero_string = zero_address.to_string();
    let recovered_zero = Address::from_str(&zero_string).unwrap();
    assert_eq!(zero_address, recovered_zero);

    // All ones (0xFF)
    let ones = [0xFFu8; 33];
    let ones_address = Address::from_bytes(ones);
    let ones_string = ones_address.to_string();
    let recovered_ones = Address::from_str(&ones_string).unwrap();
    assert_eq!(ones_address, recovered_ones);

    // Alternating pattern
    let mut alternating = [0u8; 33];
    for i in 0..33 {
        alternating[i] = if i % 2 == 0 { 0xAA } else { 0x55 };
    }
    let alt_address = Address::from_bytes(alternating);
    let alt_string = alt_address.to_string();
    let recovered_alt = Address::from_str(&alt_string).unwrap();
    assert_eq!(alt_address, recovered_alt);
}

/// Test that Address length constant is correct
#[test]
fn test_address_length_constant() {
    assert_eq!(Address::LEN, 33);

    let bytes = [0u8; 33];
    let address = Address::from_bytes(bytes);
    assert_eq!(address.as_bytes().len(), Address::LEN);
}

/// Test multiple conversions maintain consistency
#[test]
fn test_multiple_conversions() {
    let original_bytes = [200u8; 33];
    let address = Address::from_bytes(original_bytes);

    // Convert to string and back multiple times
    let mut current_address = address;
    for _ in 0..10 {
        let string = current_address.to_string();
        current_address = Address::from_str(&string).unwrap();
    }

    assert_eq!(current_address, address);
    assert_eq!(current_address.as_bytes(), address.as_bytes());
}

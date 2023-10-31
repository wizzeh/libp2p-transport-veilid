use std::sync::Arc;

use libp2p_core::Multiaddr;

use veilid_core::{CryptoKey, Encodable, FourCC, FromStr, Target, TypedKey, VeilidAPIError};
use veilid_core::{CryptoTyped, VeilidStateConfig};
pub use veilid_core::{VeilidAPI, VeilidConfig};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

use crate::VeilidError;

/// we format veilid addresses as "/unix/VLD0:Ib9pWCZfmvMYQwW8jt9XS2kCz3KdEgSriOgDP4DOtOw"
/// this parses the formatted str into a veilid target to create a direct node-to-node connection
pub fn multiaddr_to_target(m: &Multiaddr) -> Result<Target, &'static str> {
    // Parse the input string and split it into kind and value
    let s = m.to_string();

    if let Some(index) = s.find(":") {
        let kind_str = &s[6..index]; // "/unix/" takes 6 characters, so start from index 6
        let value_str = &s[index + 1..];

        // Create a CryptoTyped object
        let typed_key: TypedKey = CryptoTyped {
            kind: FourCC::try_from(kind_str.to_string()).unwrap(),
            value: CryptoKey::try_from(value_str).unwrap(),
        };

        trace!("create_target_from_str | typed_key {:?}", typed_key);

        // Create a Target::NodeId object
        Ok(Target::NodeId(typed_key))
    } else {
        Err("Invalid string format")
    }
}

pub fn cryptotyped_to_target(c: &CryptoTyped<CryptoKey>) -> Target {
    Target::NodeId(*c)
}

pub fn cryptotyped_to_multiaddr(c: &CryptoTyped<CryptoKey>) -> Multiaddr {
    trace!("cryptotyped_to_multiaddr {:?}", c);

    let kind = c.kind;

    let value = c.value.encode();

    let str = format!("/unix/{:?}:{:?}", kind, value);

    let addr: Multiaddr = str.parse().unwrap();
    trace!("cryptotyped_to_multiaddr: {:?}", addr);

    addr
}

// Create a [`Multiaddr`] from the given target.
fn _target_to_multiaddr(_target: Target) -> Multiaddr {
    todo!()
}

pub fn validate_multiaddr_for_veilid(addr: &Multiaddr) -> Result<(), VeilidError> {
    // Convert Multiaddr to string for easier parsing.
    let addr_str = addr.to_string();

    // Check if the address starts with "/unix"
    if !addr_str.starts_with("/unix/") {
        return Err(VeilidError::InvalidMultiaddr);
    }

    // Extract the Veilid ID part
    let veilid_id_part = &addr_str["/unix/".len()..];

    // Validate or use the Veilid ID part
    if veilid_id_part.is_empty() || !veilid_id_part.starts_with("VLD0:") {
        return Err(VeilidError::InvalidMultiaddr);
    }

    Ok(())
}

pub fn get_veilid_state_config(
    api: Option<Arc<VeilidAPI>>,
) -> Result<VeilidStateConfig, VeilidAPIError> {
    if let Some(api) = api {
        let result = api.config();
        match result {
            Ok(veilid_config) => {
                return Ok(*veilid_config.get_veilid_state());
            }
            Err(e) => return Err(e),
        }
    } else {
        return Err(VeilidAPIError::Generic {
            message: "Could not retrieve Veilid Config from VeilidAPI".to_string(),
        });
    }
}

pub fn get_my_node_id_from_veilid_state_config(
    veilid_state_config: VeilidStateConfig,
) -> Option<CryptoTyped<CryptoKey>> {
    // Extracting my node_id
    let veilid_config_inner = &veilid_state_config.config;
    let routing_table = &veilid_config_inner.network.routing_table;
    let crypto_typed_group = &routing_table.node_id;
    let keys = crypto_typed_group.get(FourCC::from_str("VLD0").unwrap());

    if let Some(key) = keys {
        return Some(key);
    } else {
        return None;
    }
}

// pub fn vec_to_u32(bytes: Vec<u8>) -> Result<u32, &'static str> {
//     if bytes.len() < 4 {
//         return Err("Byte vector is too short to convert to u32");
//     }

//     // Take the first 4 bytes from the vector
//     let b = &bytes[0..4];

//     // Convert the bytes to a u32
//     let value = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);

//     Ok(value)
// }

// pub fn u32_to_vec(value: u32) -> Vec<u8> {
//     value.to_le_bytes().to_vec()
// }

// #[derive(Clone, Debug)]
// pub struct TargetWrapper {
//     target: Target,
//     key: String, // This field will be used for hashing and comparison
// }

// impl TargetWrapper {
//     pub fn to_target(wrapper: TargetWrapper) -> Target {
//         wrapper.target
//     }
// }

// impl From<Target> for TargetWrapper {
//     fn from(target: Target) -> Self {
//         let key = match &target {
//             Target::NodeId(typed_key) => format!("NodeId:{}", typed_key.to_string()), // Assuming TypedKey implements ToString
//             Target::PrivateRoute(route_id) => format!("PrivateRoute:{}", route_id.to_string()), // Assuming RouteId implements ToString
//         };
//         TargetWrapper { target, key }
//     }
// }

// use std::hash::{Hash, Hasher};

// impl PartialEq for TargetWrapper {
//     fn eq(&self, other: &Self) -> bool {
//         self.key == other.key
//     }
// }

// impl Eq for TargetWrapper {}

// impl Hash for TargetWrapper {
//     fn hash<H: Hasher>(&self, state: &mut H) {
//         self.key.hash(state);
//     }
// }

// fn extract_crypto_key_from_input(input: InputAddress) -> Result<CryptoKey, VeilidError> {
//     // let input = InputAddress::StrType("/unix/VLD0:Q0Xhiv6Aqh4ICTHwtLniTHVyCHxIPBWrJkR0s9O1hX0");

//     fn parse_unix_string(s: &str) -> Option<(&str, &str)> {
//         // Remove /unix/ prefix if it exists
//         let s = if s.starts_with("/unix/") { &s[6..] } else { s };

//         // Find the index of the ':' character
//         let index = s.find(':')?;

//         // Split the string into two around the ':' character
//         let (typ, value) = s.split_at(index);

//         // Remove the ':' from the value
//         let value = &value[1..];

//         Some((typ, value))
//     }

//     match input {
//         InputAddress::StrType(str) => {
//             if let Some((typ, value)) = parse_unix_string(str) {
//                 println!("Type: {}, Value: {}", typ, value); // Output: Type: VLD0, Value: Q0Xhiv6Aqh4ICTHwtLniTHVyCHxIPBWrJkR0s9O1hX0
//                 let crypto_key: CryptoKey =
//             } else {
//                 error!("extract_crypto_key_from_input | Invalid format")
//             }
//         }
//         InputAddress::MultiaddrType(address) => {

//         }
//     }

//     todo!()
// }

// #[cfg(test)]
// mod tests {
//     use super::*;

//     #[test]
//     fn test_extract_crypto_key_from_str() {
//         let result = extract_crypto_key_from_input(InputAddress::StrType(
//             "/unix/VLD0:Q0Xhiv6Aqh4ICTHwtLniTHVyCHxIPBWrJkR0s9O1hX0",
//         ))
//         .unwrap();
//         assert_eq!(result.0, "key_from_str");
//     }

//     #[test]
//     fn test_extract_crypto_key_from_multiaddr() {
//         let multiaddr =
//             Multiaddr::from_str("/unix/VLD0:Q0Xhiv6Aqh4ICTHwtLniTHVyCHxIPBWrJkR0s9O1hX0").unwrap();
//         let result = extract_crypto_key_from_input(InputAddress::MultiaddrType(multiaddr)).unwrap();
//         assert_eq!(result.0, "key_from_multiaddr");
//     }
// }

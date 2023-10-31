use lazy_static::lazy_static;
use veilid_core::CryptoKind;
use veilid_core::CryptoTyped;
use veilid_core::KeyPair;
use veilid_core::TypedKeyGroup;
use veilid_core::CRYPTO_KIND_VLD0;

use std::any::Any;
use std::convert::From;
use std::env;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;

use veilid_core::FourCC;
use veilid_core::VeilidAPIError;

use toml::Value;

const CRYPTO_KIND: CryptoKind = CRYPTO_KIND_VLD0;

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

#[derive(Debug, PartialEq, Clone, Copy)]
enum ValueType {
    Str,
    Bool,
    OptString,
    U32,
    U8,
    OptU32,
    VecString,
    VecFourCC,
}

lazy_static! {
    static ref PARENT_DIR: PathBuf = env::current_dir().unwrap();
    static ref CONFIG_TABLE: [(&'static str, ValueType); 92] = [
        ("program_name", ValueType::Str),
        ("namespace", ValueType::Str),

        // Logging
        ("logging.api.enabled", ValueType::Bool),
        ("logging.api.level", ValueType::Str),

        // Capabilities
        ("capabilities.disable", ValueType::VecFourCC),

        // Blockstore
        ("block_store.directory", ValueType::Str),
        ("block_store.delete", ValueType::Bool),

        // Protected Store
        ("protected_store.allow_insecure_fallback", ValueType::Bool),
        (
            "protected_store.always_use_insecure_storage",
            ValueType::Bool
        ),
        ("protected_store.directory", ValueType::Str),
        ("protected_store.delete", ValueType::Bool),
        (
            "protected_store.device_encryption_key_password",
            ValueType::Str
        ),
        (
            "protected_store.new_device_encryption_key_password",
            ValueType::OptString
        ),

        // Table Store
        ("table_store.directory", ValueType::Str),
        ("table_store.delete", ValueType::Bool),


        //Network
        ("network.application.http.enabled", ValueType::Bool),
        ("network.application.http.listen_address", ValueType::Str),
        ("network.application.http.path", ValueType::Str),
        ("network.application.http.url", ValueType::OptString),

        ("network.application.https.enabled", ValueType::Bool),
        ("network.application.https.listen_address", ValueType::Str),
        ("network.application.https.path", ValueType::Str),
        ("network.application.https.url", ValueType::OptString),

        ("network.client_whitelist_timeout_ms", ValueType::U32),

        ("network.connection_initial_timeout_ms", ValueType::U32),
        ("network.connection_inactivity_timeout_ms", ValueType::U32),

        ("network.detect_address_changes", ValueType::Bool),

        ("network.dht.max_find_node_count", ValueType::U32),
        ("network.dht.resolve_node_timeout_ms", ValueType::U32),
        ("network.dht.resolve_node_count", ValueType::U32),
        ("network.dht.resolve_node_fanout", ValueType::U32),
        ("network.dht.get_value_timeout_ms", ValueType::U32),
        ("network.dht.get_value_count", ValueType::U32),
        ("network.dht.get_value_fanout", ValueType::U32),
        ("network.dht.set_value_timeout_ms", ValueType::U32),
        ("network.dht.set_value_count", ValueType::U32),
        ("network.dht.set_value_fanout", ValueType::U32),
        ("network.dht.min_peer_count", ValueType::U32),
        ("network.dht.min_peer_refresh_time_ms", ValueType::U32),
        ("network.dht.validate_dial_info_receipt_time_ms", ValueType::U32),
        ("network.dht.local_subkey_cache_size", ValueType::U32),
        ("network.dht.local_max_subkey_cache_memory_mb", ValueType::U32),
        ("network.dht.remote_subkey_cache_size", ValueType::U32),
        ("network.dht.remote_max_records", ValueType::U32),
        ("network.dht.remote_max_subkey_cache_memory_mb", ValueType::U32),
        ("network.dht.remote_max_storage_space_mb", ValueType::U32),

        ("network.hole_punch_receipt_time_ms", ValueType::U32),

        ("network.max_connections_per_ip4", ValueType::U32),
        ("network.max_connections_per_ip6_prefix", ValueType::U32),
        ("network.max_connections_per_ip6_prefix_size",ValueType::U32),
        ("network.max_connection_frequency_per_min",ValueType::U32),

        ("network.network_key_password", ValueType::OptString ),

        ("network.protocol.tcp.connect", ValueType::Bool),
        ("network.protocol.tcp.listen", ValueType::Bool),
        ("network.protocol.tcp.max_connections", ValueType::U32),
        ("network.protocol.tcp.listen_address", ValueType::Str),
        ("network.protocol.tcp.public_address", ValueType::OptString),

        ("network.protocol.udp.enabled", ValueType::Bool),
        ("network.protocol.udp.socket_pool_size", ValueType::U32),
        ("network.protocol.udp.listen_address", ValueType::Str),
        ("network.protocol.udp.public_address", ValueType::OptString),

        ("network.protocol.ws.connect", ValueType::Bool),
        ("network.protocol.ws.listen", ValueType::Bool),
        ("network.protocol.ws.listen_address", ValueType::Str),
        ("network.protocol.ws.max_connections", ValueType::U32),
        ("network.protocol.ws.path", ValueType::Str),
        ("network.protocol.ws.url", ValueType::OptString),

        ("network.protocol.wss.connect", ValueType::Bool),
        ("network.protocol.wss.listen", ValueType::Bool),
        ("network.protocol.wss.listen_address", ValueType::Str),
        ("network.protocol.wss.max_connections", ValueType::U32),
        ("network.protocol.wss.path", ValueType::Str),
        ("network.protocol.wss.url", ValueType::OptString),

        ("network.restricted_nat_retries", ValueType::U32),

        ("network.reverse_connection_receipt_time_ms", ValueType::U32),

        ("network.routing_table.bootstrap", ValueType::VecString),
        ("network.routing_table.limit_over_attached", ValueType::U32),
        ("network.routing_table.limit_fully_attached", ValueType::U32),
        ("network.routing_table.limit_attached_strong", ValueType::U32),
        ("network.routing_table.limit_attached_good", ValueType::U32),
        ("network.routing_table.limit_attached_weak", ValueType::U32),

        ("network.rpc.concurrency", ValueType::U32),
        ("network.rpc.default_route_hop_count", ValueType::U8),
        ("network.rpc.max_timestamp_behind_ms", ValueType::OptU32),
        ("network.rpc.max_timestamp_ahead_ms", ValueType::OptU32),
        ("network.rpc.max_route_hop_count", ValueType::U8),
        ("network.rpc.queue_size", ValueType::U32),
        ("network.rpc.timeout_ms", ValueType::U32),

        ("network.tls.certificate_path", ValueType::Str),
        ("network.tls.connection_initial_timeout_ms", ValueType::U32),
        ("network.tls.private_key_path", ValueType::Str),

        ("network.upnp", ValueType::Bool),

    ];
}

const DEFAULT_SETTINGS: &str = include_str!("./settings.toml");

pub fn lookup_config(config_key: &str) -> Result<Box<dyn Any + Send>, VeilidAPIError> {
    let path = PARENT_DIR.join("./.veilid/settings");

    std::fs::create_dir_all(path.parent().unwrap())
        .map_err(|e| VeilidAPIError::generic(format!("Failed to create directory: {}", e)))?;

    if !path.exists() {
        // Write the default settings to the file.
        let mut file = File::create(&*path).map_err(|e| {
            VeilidAPIError::generic(format!("Failed to create settings file: {}", e))
        })?;

        file.write_all(DEFAULT_SETTINGS.as_bytes()).map_err(|e| {
            VeilidAPIError::generic(format!(
                "Failed to write default settings to settings.toml: {}",
                e
            ))
        })?;
    }

    let toml_str = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            return Err(VeilidAPIError::generic(
                "Failed to read settings.toml".to_string(),
            ));
        }
    };

    let value: Value = match toml_str.parse() {
        Ok(v) => v,
        Err(e) => {
            return Err(VeilidAPIError::generic(format!(
                "Failed to parse toml: {}",
                e
            )));
        }
    };

    let mut current_value: Option<&Value> = Some(&value);

    for key_part in config_key.split('.') {
        if let Some(v) = current_value {
            current_value = v.get(key_part);
        } else {
            return Err(VeilidAPIError::generic(format!(
                "Config key '{}' not found in TOML",
                config_key
            )));
        }
    }

    let config_value = current_value;

    if config_value.is_none() {
        trace!("config_value.is_none() {:?}", config_key);

        match config_key {
            "network.routing_table.node_id" => match manage_keypair() {
                Some(key_pair) => {
                    let mut group = TypedKeyGroup::new();

                    group.add(veilid_core::CryptoTyped::new(
                        CRYPTO_KIND,
                        key_pair.value.key,
                    ));
                    return Ok(Box::new(group));
                }
                None => {
                    return Err(VeilidAPIError::generic(format!(
                        "Failed to retreive keypair"
                    )));
                }
            },
            "network.routing_table.node_id_secret" => match manage_keypair() {
                Some(key_pair) => {
                    let mut group = TypedKeyGroup::new();

                    group.add(veilid_core::CryptoTyped::new(
                        CRYPTO_KIND,
                        key_pair.value.secret,
                    ));
                    return Ok(Box::new(group));
                }
                None => {
                    return Err(VeilidAPIError::generic(format!(
                        "Failed to retreive keypair"
                    )));
                }
            },
            _ => {
                return Err(VeilidAPIError::generic(format!(
                    "Config key '{}' not found in TOML",
                    config_key
                )));
            }
        }
    }

    match get_value_type(config_key)? {
        ValueType::Str => Ok(Box::new(
            config_value
                .unwrap()
                .as_str()
                .ok_or_else(|| VeilidAPIError::generic("Expected a string value in TOML"))?
                .to_string(),
        )),

        ValueType::Bool => {
            Ok(Box::new(config_value.unwrap().as_bool().ok_or_else(
                || VeilidAPIError::generic("Expected a boolean value in TOML"),
            )?))
        }

        ValueType::OptString => {
            if let Some(str_val) = config_value.unwrap().as_str() {
                match str_val {
                    "" => Ok(Box::new(None::<String>)),
                    _ => Ok(Box::new(Some(str_val.to_string()))),
                }
            } else {
                Ok(Box::new(Option::<String>::None))
            }
        }

        ValueType::U32 => Ok(Box::new(
            config_value
                .unwrap()
                .as_integer()
                .ok_or_else(|| VeilidAPIError::generic("Expected a u32 value in TOML"))?
                as u32,
        )),

        ValueType::U8 => Ok(Box::new(
            config_value
                .unwrap()
                .as_integer()
                .ok_or_else(|| VeilidAPIError::generic("Expected a u8 value in TOML"))?
                as u8,
        )),

        ValueType::OptU32 => {
            if let Some(int_val) = config_value.unwrap().as_integer() {
                Ok(Box::new(Some(int_val as u32)))
            } else {
                Ok(Box::new(Option::<u32>::None))
            }
        }

        ValueType::VecString => {
            let arr = config_value
                .unwrap()
                .as_array()
                .ok_or_else(|| VeilidAPIError::generic("Expected an array of strings in TOML"))?;
            let vec = arr
                .iter()
                .filter_map(|val| val.as_str())
                .map(|s| s.to_string())
                .collect::<Vec<String>>();
            Ok(Box::new(vec))
        }

        ValueType::VecFourCC => {
            let arr = config_value.unwrap().as_array().ok_or_else(|| {
                VeilidAPIError::generic("Expected an array of strings for VecFourCC in TOML")
            })?;
            let vec = arr
                .iter()
                .filter_map(|val| val.as_str())
                .filter_map(string_to_fourcc)
                .map(FourCC::from)
                .collect::<Vec<FourCC>>();
            Ok(Box::new(vec))
        }
    }
}

fn get_value_type(key: &str) -> Result<ValueType, VeilidAPIError> {
    for &(entry_key, entry_value) in &*CONFIG_TABLE {
        if entry_key == key {
            trace!("get_value_type: {:?}: {:?}", key, entry_value);
            return Ok(entry_value);
        }
    }
    Err(VeilidAPIError::generic(format!(
        "get_value_type | key not found '{}'",
        key
    )))
}

fn string_to_fourcc(input: &str) -> Option<u32> {
    if input.len() != 4 {
        return None;
    }

    let bytes = input.as_bytes();
    let fourcc: u32 = (bytes[0] as u32) << 24
        | (bytes[1] as u32) << 16
        | (bytes[2] as u32) << 8
        | (bytes[3] as u32);

    Some(fourcc)
}

pub fn manage_keypair() -> Option<CryptoTyped<KeyPair>> {
    let parent_dir = PARENT_DIR.join("./.veilid");

    let keypair_file = parent_dir.join("keys"); // Place the keypair file in that directory

    // Create parent directory
    if !parent_dir.exists() {
        if let Err(e) = fs::create_dir_all(&*parent_dir) {
            error!("Failed to create directory: {:?}", e);
        }
    }

    let mut key_pair: Option<CryptoTyped<KeyPair>> = None;

    // Read existing key pair if exists
    if keypair_file.exists() {
        match fs::read_to_string(&keypair_file) {
            Ok(data) => match data.trim().parse() {
                Ok(parsed_key) => key_pair = Some(parsed_key),
                Err(_) => error!("Can't parse key pair"),
            },
            Err(e) => {
                error!("Unable to read key pair file: {:?}", e);
            }
        }
    }

    // Generate new key pair if none exists
    if key_pair.is_none() {
        match veilid_core::Crypto::generate_keypair(CRYPTO_KIND) {
            Ok(generated_key) => key_pair = Some(generated_key),
            Err(_) => error!("Cannot generate key pair"),
        }

        if let Some(ref kp) = key_pair {
            if let Err(e) = fs::write(&keypair_file, kp.to_string()) {
                error!("Unable to write key pair to file: {:?}", e);
            }
        }
    }

    let key_pair = match key_pair {
        Some(kp) => kp,
        None => {
            // let e = io::Error::new(io::ErrorKind::Other, "Key pair should exist by now");
            // let e = VeilidError::KeyPairDoesntExist;
            error!("Key pair does not exist");
            todo!()
        }
    };

    trace!("Veilid Keypair: {:?}", key_pair);
    Some(key_pair)
}

// pub fn handle_config(key: String) -> ConfigCallbackReturn {
//     // let temp_dir = "../.veilid";
//     let temp_dir: PathBuf = env::current_dir().unwrap().join("./.tickle/.veilid");

//     let key_pair = manage_keypair().unwrap();

//     match key.as_str() {
//         "program_name" => Ok(Box::new(String::from("tickle"))),
//         "namespace" => Ok(Box::<String>::default()),

//         "logging.api.enabled" => Ok(Box::new(true)),
//         "logging.api.level" => Ok(Box::new(String::from("trace"))),

//         "capabilities.disable" => Ok(Box::<Vec<FourCC>>::default()),

//         "table_store.directory" => Ok(Box::new(
//             temp_dir
//                 .join("table")
//                 .to_str()
//                 .as_ref()
//                 .context("invalid path")
//                 .map_err(|e| VeilidAPIError::Generic {
//                     message: e.to_string(),
//                 })?
//                 .to_string(),
//         )),
//         "table_store.delete" => Ok(Box::new(true)),

//         "block_store.directory" => Ok(Box::new(
//             temp_dir
//                 .join("block")
//                 .to_str()
//                 .as_ref()
//                 .context("invalid path")
//                 .map_err(|e| VeilidAPIError::Generic {
//                     message: e.to_string(),
//                 })?
//                 .to_string(),
//         )),
//         "block_store.delete" => Ok(Box::new(true)),

//         "protected_store.allow_insecure_fallback" => Ok(Box::new(true)),
//         "protected_store.always_use_insecure_storage" => Ok(Box::new(true)),
//         "protected_store.directory" => Ok(Box::new(
//             temp_dir
//                 .join("protected")
//                 .to_str()
//                 .as_ref()
//                 .context("invalid path")
//                 .map_err(|e| VeilidAPIError::Generic {
//                     message: e.to_string(),
//                 })?
//                 .to_string(),
//         )),
//         "protected_store.delete" => Ok(Box::new(true)),
//         "protected_store.device_encryption_key_password" => Ok(Box::new("".to_owned())),
//         "protected_store.new_device_encryption_key_password" => {
//             Ok(Box::new(Option::<String>::None))
//         }

//         //Network
//         "network.application.http.enabled" => Ok(Box::new(false)),
//         "network.application.http.listen_address" => Ok(Box::new(String::from(""))),
//         "network.application.http.path" => Ok(Box::new(String::from("app"))),
//         "network.application.http.url" => Ok(Box::new(Option::<String>::None)),

//         "network.application.https.enabled" => Ok(Box::new(false)),
//         "network.application.https.listen_address" => Ok(Box::new(String::from(""))),
//         "network.application.https.path" => Ok(Box::new(String::from("app"))),
//         "network.application.https.url" => Ok(Box::new(Option::<String>::None)),

//         "network.client_whitelist_timeout_ms" => Ok(Box::new(300_000u32)),

//         "network.connection_initial_timeout_ms" => Ok(Box::new(2_000u32)),
//         "network.connection_inactivity_timeout_ms" => Ok(Box::new(60_000u32)),

//         "network.detect_address_changes" => Ok(Box::new(true)),

//         "network.dht.max_find_node_count" => Ok(Box::new(20u32)),
//         "network.dht.resolve_node_timeout_ms" => Ok(Box::new(10_000u32)),
//         "network.dht.resolve_node_count" => Ok(Box::new(1u32)),
//         "network.dht.resolve_node_fanout" => Ok(Box::new(4u32)),
//         "network.dht.get_value_timeout_ms" => Ok(Box::new(10_000u32)),
//         "network.dht.get_value_count" => Ok(Box::new(3u32)),
//         "network.dht.get_value_fanout" => Ok(Box::new(4u32)),
//         "network.dht.set_value_timeout_ms" => Ok(Box::new(10_000u32)),
//         "network.dht.set_value_count" => Ok(Box::new(5u32)),
//         "network.dht.set_value_fanout" => Ok(Box::new(4u32)),
//         "network.dht.min_peer_count" => Ok(Box::new(20u32)),
//         "network.dht.min_peer_refresh_time_ms" => Ok(Box::new(60_000u32)),
//         "network.dht.validate_dial_info_receipt_time_ms" => Ok(Box::new(2_000u32)),
//         "network.dht.local_subkey_cache_size" => Ok(Box::new(128u32)),
//         "network.dht.local_max_subkey_cache_memory_mb" => Ok(Box::new(256u32)),
//         "network.dht.remote_subkey_cache_size" => Ok(Box::new(1024u32)),
//         "network.dht.remote_max_records" => Ok(Box::new(65536u32)),
//         "network.dht.remote_max_subkey_cache_memory_mb" => Ok(Box::new(64u32)),
//         "network.dht.remote_max_storage_space_mb" => Ok(Box::new(1000u32)),

//         "network.hole_punch_receipt_time_ms" => Ok(Box::new(5_000u32)),

//         "network.max_connections_per_ip4" => Ok(Box::new(32u32)),
//         "network.max_connections_per_ip6_prefix" => Ok(Box::new(32u32)),
//         "network.max_connections_per_ip6_prefix_size" => Ok(Box::new(56u32)),
//         "network.max_connection_frequency_per_min" => Ok(Box::new(128u32)),

//         "network.network_key_password" => Ok(Box::new(Option::<String>::None)),

//         "network.protocol.tcp.connect" => Ok(Box::new(true)),
//         "network.protocol.tcp.listen" => Ok(Box::new(true)),
//         "network.protocol.tcp.max_connections" => Ok(Box::new(32u32)),
//         "network.protocol.tcp.listen_address" => Ok(Box::new("".to_owned())),
//         "network.protocol.tcp.public_address" => Ok(Box::new(Option::<String>::None)),

//         "network.protocol.udp.enabled" => Ok(Box::new(true)),
//         "network.protocol.udp.socket_pool_size" => Ok(Box::new(0u32)),
//         "network.protocol.udp.listen_address" => Ok(Box::new("".to_owned())),
//         "network.protocol.udp.public_address" => Ok(Box::new(Option::<String>::None)),

//         "network.protocol.ws.connect" => Ok(Box::new(false)),
//         "network.protocol.ws.listen" => Ok(Box::new(false)),
//         "network.protocol.ws.listen_address" => Ok(Box::new("".to_owned())),
//         "network.protocol.ws.max_connections" => Ok(Box::new(16u32)),
//         "network.protocol.ws.path" => Ok(Box::new(String::from("ws"))),
//         "network.protocol.ws.url" => Ok(Box::new(Option::<String>::None)),

//         "network.protocol.wss.connect" => Ok(Box::new(false)),
//         "network.protocol.wss.listen" => Ok(Box::new(false)),
//         "network.protocol.wss.listen_address" => Ok(Box::new("".to_owned())),
//         "network.protocol.wss.max_connections" => Ok(Box::new(16u32)),
//         "network.protocol.wss.path" => Ok(Box::new(String::from("ws"))),
//         "network.protocol.wss.url" => Ok(Box::new(Option::<String>::None)),

//         "network.restricted_nat_retries" => Ok(Box::new(0u32)),

//         "network.reverse_connection_receipt_time_ms" => Ok(Box::new(5_000u32)),

//         "network.routing_table.node_id" => {
//             let mut group = TypedKeyGroup::new();
//             group.add(veilid_core::CryptoTyped::new(
//                 CRYPTO_KIND,
//                 key_pair.value.key,
//             ));
//             Ok(Box::new(group))
//         }
//         "network.routing_table.node_id_secret" => {
//             let mut group = TypedSecretGroup::new();
//             group.add(veilid_core::CryptoTyped::new(
//                 CRYPTO_KIND,
//                 key_pair.value.secret,
//             ));
//             Ok(Box::new(group))
//         }
//         "network.routing_table.bootstrap" => Ok(Box::new(vec![
//             "bootstrap.veilid.net".to_string(),
//             "ws://bootstrap.veilid.net:5150/ws".to_string(),
//         ])),
//         "network.routing_table.limit_over_attached" => Ok(Box::new(64u32)),
//         "network.routing_table.limit_fully_attached" => Ok(Box::new(32u32)),
//         "network.routing_table.limit_attached_strong" => Ok(Box::new(16u32)),
//         "network.routing_table.limit_attached_good" => Ok(Box::new(8u32)),
//         "network.routing_table.limit_attached_weak" => Ok(Box::new(4u32)),

//         "network.rpc.concurrency" => Ok(Box::new(0u32)),
//         "network.rpc.default_route_hop_count" => Ok(Box::new(1u8)),
//         "network.rpc.max_timestamp_behind_ms" => Ok(Box::new(Some(10_000u32))),
//         "network.rpc.max_timestamp_ahead_ms" => Ok(Box::new(Some(10_000u32))),
//         "network.rpc.max_route_hop_count" => Ok(Box::new(4u8)),
//         "network.rpc.queue_size" => Ok(Box::new(1024u32)),
//         "network.rpc.timeout_ms" => Ok(Box::new(5_000u32)),

//         "network.tls.certificate_path" => Ok(Box::new(
//             temp_dir
//                 .join("cert")
//                 .to_str()
//                 .as_ref()
//                 .context("invalid path")
//                 .map_err(|e| VeilidAPIError::Generic {
//                     message: e.to_string(),
//                 })?
//                 .to_string(),
//         )),
//         "network.tls.connection_initial_timeout_ms" => Ok(Box::new(2_000u32)),
//         "network.tls.private_key_path" => Ok(Box::new(
//             temp_dir
//                 .join("key")
//                 .to_str()
//                 .as_ref()
//                 .context("invalid path")
//                 .map_err(|e| VeilidAPIError::Generic {
//                     message: e.to_string(),
//                 })?
//                 .to_string(),
//         )),

//         "network.upnp" => Ok(Box::new(true)),

//         _ => {
//             let err = format!("config key '{}' doesn't exist", key);
//             error!("{}", err);
//             Err(VeilidAPIError::internal(err))
//         }
//     }
// }

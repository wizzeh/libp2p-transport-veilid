use veilid_core::{CryptoKey, FourCC, FromStr};
use veilid_core::{CryptoTyped, VeilidStateConfig};
pub use veilid_core::{VeilidAPI, VeilidConfig};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

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

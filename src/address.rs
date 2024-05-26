use chrono::{DateTime, Utc};
use libp2p::Multiaddr;
use std::sync::Arc;
use veilid_core::{CryptoKey, CryptoTyped, DHTSchema, Encodable, FourCC, KeyPair, Sequencing, Target, VeilidAPI};

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum DHTStatus {
    None,
    Active(DateTime<Utc>),
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
pub enum Address {
    Safe(CryptoTyped<CryptoKey>, Option<KeyPair>, DHTStatus),
    Unsafe(CryptoTyped<CryptoKey>),
}


impl Address {
    pub fn new_unsafe(key: &CryptoTyped<CryptoKey>) -> Address {
        Address::Unsafe(*key)
    }

    pub async fn new_safe(api: &Arc<VeilidAPI>) -> Result<Address, String> {
        debug!("Address::new_safe");
        let api = api.clone();

        match api.new_custom_private_route(&vec![FourCC(*b"VLD0")], veilid_core::Stability::LowLatency, Sequencing::NoPreference).await {
            Err(e) => error!("Address::new_safe | new_private_route {:?}", e),
            Ok(private_route) => {
                let blob_data = private_route.1;
                trace!(
                    "Address::new_safe | private_route_blob {:?}",
                    blob_data.len()
                );

                match api.routing_context() {
                    Err(e) => {
                        error!("Address::new_safe | api.routing_context() | {:?}", e)
                    }
                    Ok(routing_context) => match DHTSchema::dflt(1) {
                        Err(e) => {
                            error!("Address::new_safe | DHTSchema::dflt(1) | {:?}", e)
                        }
                        Ok(schema) => match routing_context.clone().with_sequencing(Sequencing::NoPreference).create_dht_record(schema, None).await {
                            Err(e) => error!("Address::new_safe | DHTSchema::dflt(1) | {:?}", e),
                            Ok(dht_record_descriptor) => {
                                trace!(
                                    "Address::new_safe | dht_record_descriptor {:?}",
                                    dht_record_descriptor
                                );

                                let public_key = dht_record_descriptor.owner();

                                if let Some(secret_key) = dht_record_descriptor.owner_secret() {

                                    let keypair = KeyPair {
                                        key: *public_key,
                                        secret: *secret_key,
                                    };

                                    let dht_key = dht_record_descriptor.key();
                                    trace!("Address::new_safe | key {:?}", dht_key);
    
                                    match routing_context.with_sequencing(Sequencing::NoPreference).set_dht_value(dht_key.clone(), 0, blob_data.clone(), None).await {
                                        Err(e) => error!("Address::new_safe | routing_context.set_dht_value() | {:?}", e),
                                        Ok(_) =>  {
                                            debug!("Address::new_safe | routing_context.set_dht_value | success");
    
                                            return Ok(Address::Safe(*dht_key, Some(keypair), DHTStatus::Active(Utc::now())));
    
                                        },
                                    }
                                } else {
                                    error!("Address::new_safe | failed to get owner_secret");
                                }
                            }
                        },
                    },
                }
            }
        }

        Err("Failed to create new safe address".to_string())
    }

    pub async fn update_safe(api: &Arc<VeilidAPI>, address: Address) -> Result<Target, String> {
        let api = api.clone();

        match address {
            Address::Safe(key, keypair, ..) => {
                match api.new_custom_private_route(&vec![FourCC(*b"VLD0")], veilid_core::Stability::LowLatency, Sequencing::NoPreference).await {
                    Err(e) => error!("Address::update_safe | new_private_route {:?}", e),
                    Ok(private_route) => {
    
                        if let Some(owner_keys) = keypair {
                            let blob_data = private_route.1;
                            trace!(
                                "Address::update_safe | private_route_blob {:?}",
                                blob_data.len()
                            );
        
                            match api.routing_context() {
                                Err(e) => {
                                    error!("Address::update_safe | failed to get routing_context | {:?}", e)
                                }
                                Ok(routing_context) => {
                                    if let Ok(dht_record) = routing_context.clone().with_sequencing(Sequencing::NoPreference).open_dht_record(key, Some(owner_keys)).await
                                    {
                                        trace!("Address::update_safe | open_dht_record {:?}", dht_record);
        
                                        if let Ok(result) =
                                            routing_context.with_sequencing(Sequencing::NoPreference).set_dht_value(key, 0, blob_data.clone(), Some(owner_keys)).await
                                        {
                                            trace!(
                                                "Address::update_safe | set_dht_value | key {:?}",
                                                result
                                            );
                                            debug!(
                                                "Address::update_safe | successfully updated DHT with new target",
                                            );

                                            if let Ok(target) = api.import_remote_private_route(blob_data){
                           
                                                let new_target = Target::PrivateRoute(target);
                                                warn!(
                                                    "Address::update_safe | new target {:?}", new_target
                                                );
                                                return Ok(new_target);
                                            } else {
                                                error!("Address::update_safe | failed to import private route blob");
                                            }
                                        } else {
                                            error!("Address::update_safe | failed to set_dht_value");
                                        }
                                    } else {
                                        error!("Address::update_safe | failed to open_dht_record");
                                    }
                                }
                            }
                        } else {
                            error!("Address::update_safe | owner keys not provided");
                        }
                    },
                };
            }
            
            Address::Unsafe(_) => return Err("Can't update an unsafe address".to_string()),
        };

        Err("Failed to update address".to_string())        
    }

    pub async fn to_target(&self, api: &Arc<VeilidAPI>) -> Option<Target> {
        let mut target = None;

        match self {
            Address::Unsafe(key) => {
                target = Some(Target::NodeId(*key));
            }

            Address::Safe(key, ..) => {

                if let Ok(routing_context) = api.routing_context() {
                    trace!("Address::to_target got routing_context");

                    if let Ok(dht_record) = routing_context.open_dht_record(*key, None).await {
                        trace!("Address::to_target | open_dht_record {:?}", dht_record);

                        if let Ok(option_value_data) =
                            routing_context.get_dht_value(*key, 0, true).await
                        {
                            trace!(
                                "Address::to_target | option_value_data {:?}",
                                option_value_data
                            );

                            if let Some(private_route) = option_value_data {
                                if let Ok(crypto_key) =
                                    api.import_remote_private_route(private_route.data().to_vec())
                                {
                                    target = Some(Target::PrivateRoute(crypto_key));
                                    info!("Address::to_target | key {:?} | target {:?}", key.value, crypto_key);

                                } else {
                                    error!(
                                        "Address::to_target | failed import_remote_private_route"
                                    );
                                }
                            } else {
                                error!("Address::to_target | failed to get private route");
                            }
                        } else {
                            error!("Address::to_target | failed to get_dht_value");
                        }
                    } else {
                        debug!("Address::to_target | failed to open dht record");
                    }
                } else {
                    error!("Address::to_target | failed to getrouting_context");
                }
            }
        }

        target
    }

    pub fn to_multiaddr(&self) -> Multiaddr {
        match self {
            Address::Safe(key, ..) => {
                trace!("Address::to_multiaddr {:?}", key);
                let value = key.value.encode();
                let str = format!("/unix/s:{}", value);
                let addr: Multiaddr = str.parse().unwrap();
                trace!("Address::to_multiaddr | Safe | {:?}", addr);
                addr
            }
            Address::Unsafe(key) => {
                trace!("Address::to_multiaddr {:?}", key);
                let value = key.value.encode();
                let str = format!("/unix/us:{}", value);
                let addr: Multiaddr = str.parse().unwrap();
                debug!("Address::to_multiaddr | Unsafe | {:?}", addr);
                addr
            }
        }
    }

    pub fn to_key(&self) -> CryptoKey {
        match self {
            Address::Unsafe(key) => return key.value.clone(),
            Address::Safe(key, _, _) => return key.value.clone()
        }

    }

    pub fn cmp(&self, other: &Address) -> std::cmp::Ordering {
        let v1 = match self {
            Address::Safe(key, ..) => key.to_string(),
            Address::Unsafe(key) => key.to_string(),
        };

        let v2 = match other {
            Address::Safe(key, ..) => key.to_string(),
            Address::Unsafe(key) => key.to_string(),
        };

        return v1.cmp(&v2);
    }

}

/// Target::NodeId addresses are like "/unix/us:Ib9pWCZfmvMYQwW8jt9XS2kCz3KdEgSriOgDP4DOtOw"
/// Target::PrivateRoute addresses are like "/unix/s:Ib9pWCZfmvMYQwW8jt9XS2kCz3KdEgSriOgDP4DOtOw"
/// this parses the Multiaddr into a veilid Target
impl TryFrom<Multiaddr> for Address {
    type Error = String;

    fn try_from(m: Multiaddr) -> Result<Self, Self::Error> {
        let addr_str = m.to_string();
        let parts: Vec<&str> = addr_str.split(':').collect();
        if parts.len() != 2 {
            return Err("Invalid format".to_string());
        }

        let key =
            CryptoKey::try_from(parts[1]).map_err(|_| "Failed to parse CryptoKey".to_string())?;
        let typed_key = CryptoTyped::new("VLD0".as_bytes().try_into().unwrap(), key); // Consider improving error handling for "try_into"

        match parts[0] {
            "/unix/s" => Ok(Address::Safe(typed_key, None, DHTStatus::None)),
            "/unix/us" => Ok(Address::Unsafe(typed_key)),
            _ => Err("Unknown address type".to_string()),
        }
    }
}

/// this parses the String into a veilid Target
impl TryFrom<String> for Address {
    type Error = String;

    fn try_from(addr_str: String) -> Result<Self, Self::Error> {
        let parts: Vec<&str> = addr_str.split(':').collect();
        if parts.len() != 2 {
            return Err("Invalid format".to_string());
        }

        let key =
            CryptoKey::try_from(parts[1]).map_err(|_| "Failed to parse CryptoKey".to_string())?;
        let typed_key = CryptoTyped::new("VLD0".as_bytes().try_into().unwrap(), key); // Improve error handling here

        match parts[0] {
            "/unix/s" => Ok(Address::Safe(typed_key, None, DHTStatus::None)),
            "/unix/us" => Ok(Address::Unsafe(typed_key)),
            _ => Err("Unknown address type".to_string()),
        }
    }
}

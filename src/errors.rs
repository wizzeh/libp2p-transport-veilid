use std::io;

#[derive(Debug)]
pub enum VeilidError {
    IoError(io::Error),
    VeilidApiError(veilid_core::VeilidAPIError),
    CallbackNotDefined,
    CouldNotCreateListener,
    KeyPairDoesntExist,
    APINotDefined,
    InvalidMultiaddr,
    StreamNotFound,
    Generic(String),
}

impl From<veilid_core::VeilidAPIError> for VeilidError {
    fn from(err: veilid_core::VeilidAPIError) -> Self {
        VeilidError::VeilidApiError(err)
    }
}

impl std::error::Error for VeilidError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            VeilidError::IoError(err) => Some(err),
            VeilidError::VeilidApiError(err) => Some(err),
            VeilidError::CallbackNotDefined => None,
            VeilidError::CouldNotCreateListener => None,
            VeilidError::KeyPairDoesntExist => None,
            VeilidError::APINotDefined => None,
            VeilidError::InvalidMultiaddr => None,
            VeilidError::StreamNotFound => None,
            VeilidError::Generic(_) => None,
        }
    }
}

impl std::fmt::Display for VeilidError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            VeilidError::IoError(err) => write!(f, "IO error: {}", err),
            VeilidError::VeilidApiError(err) => write!(f, "Veilid API error: {}", err),
            VeilidError::CallbackNotDefined => write!(f, "Callback is not defined"),
            VeilidError::CouldNotCreateListener => write!(f, "Could not create a Libp2p Listener"),
            VeilidError::KeyPairDoesntExist => write!(f, "Key pair doesn't not exist"),
            VeilidError::APINotDefined => write!(f, "VeilidAPI does not exist"),
            VeilidError::InvalidMultiaddr => write!(f, "Multiaddr is not a unix formatted address"),
            VeilidError::StreamNotFound => write!(f, "Multiaddr is not a unix formatted address"),
            VeilidError::Generic(msg) => write!(f, "{:?}", msg),
        }
    }
}

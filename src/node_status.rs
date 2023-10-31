use lazy_static::lazy_static;
// use serde::{Deserialize, Serialize};
// use thiserror::Error;
use tokio_crate::sync::Mutex;

#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use veilid_core::{CryptoKey, CryptoTyped};

type Result<T> = std::result::Result<T, NodeStatusError>;

lazy_static! {
    pub static ref NODE_STATUS: Mutex<NodeStatus> = Mutex::new(Default::default());
}

#[derive(Debug, Default, Clone)]
pub struct NodeStatus {
    my_node_id: Option<CryptoTyped<CryptoKey>>,
    attach_state: Option<veilid_core::AttachmentState>,
    public_internet_ready: bool,
}

#[derive(Debug)]
pub enum NodeStatusError {
    _GeneralError(String),
    // Add other error variants here as needed
}

impl NodeStatus {
    pub fn _new() -> Self {
        Default::default()
    }

    // getter for my_node_id
    pub fn my_node_id(&self) -> &Option<CryptoTyped<CryptoKey>> {
        &self.my_node_id
    }

    pub fn attach_state(&self) -> &Option<veilid_core::AttachmentState> {
        &self.attach_state
    }

    pub fn public_internet_ready(&self) -> &bool {
        &self.public_internet_ready
    }

    pub fn update_my_node_id(mut self, node_id: &CryptoTyped<CryptoKey>) -> Self {
        trace!("NodeStatus::update_node_id");
        self.my_node_id = Some(node_id.to_owned());
        self
    }

    pub fn update_attach_state(mut self, state: &veilid_core::AttachmentState) -> Self {
        trace!("NodeStatus::update_attach_state");
        self.attach_state = Some(state.to_owned());
        self
    }

    pub fn update_public_internet_ready(mut self, public_internet_ready: &bool) -> Self {
        trace!("NodeStatus::update_public_internet_ready");
        self.public_internet_ready = public_internet_ready.to_owned();
        self
    }

    pub async fn get() -> Result<Self> {
        let node_status = NODE_STATUS.lock().await;
        Ok(node_status.clone())
    }

    // Asynchronous setter
    pub async fn set(&self) -> Result<()> {
        debug!("NodeStatus::set_status | start {:?}", self);

        let mut node_status = NODE_STATUS.lock().await;
        *node_status = self.clone();

        Ok(())
    }
}

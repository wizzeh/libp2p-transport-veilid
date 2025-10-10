#[allow(unused_imports)]
use log::{debug, error, info, trace, warn};
use veilid_core::{CryptoTyped, NodeId};

#[derive(Debug, Default, Clone)]
pub struct NodeStatus {
    my_node_id: Option<CryptoTyped<NodeId>>,
    is_attached: bool,
    public_internet_ready: bool,
}

impl NodeStatus {
    pub fn new() -> Self {
        Self {
            my_node_id: None,
            is_attached: false,
            public_internet_ready: false,
        }
    }

    // getter for my_node_id
    pub fn my_node_id(&self) -> &Option<CryptoTyped<NodeId>> {
        &self.my_node_id
    }

    pub fn is_attached(&self) -> &bool {
        &self.is_attached
    }

    pub fn public_internet_ready(&self) -> &bool {
        &self.public_internet_ready
    }

    pub fn update_my_node_id(&mut self, node_id: &CryptoTyped<NodeId>) -> &mut Self {
        trace!("NodeStatus::update_node_id");
        self.my_node_id = Some(node_id.to_owned());
        self
    }

    pub fn update_is_attached(&mut self, state: &bool) -> &mut Self {
        trace!("NodeStatus::update_attach_state");
        self.is_attached = *state;
        self
    }

    pub fn update_public_internet_ready(&mut self, public_internet_ready: &bool) -> &mut Self {
        trace!("NodeStatus::update_public_internet_ready");
        self.public_internet_ready = public_internet_ready.to_owned();
        self
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Payload {
    #[prost(uint32, tag="1")]
    pub received_seq: u32,
    #[prost(uint32, tag="2")]
    pub msg_seq: u32,
    #[prost(uint64, tag="3")]
    pub stream_id: u64,
    #[prost(bytes="vec", tag="4")]
    pub signature: ::prost::alloc::vec::Vec<u8>,
    #[prost(string, tag="5")]
    pub address: ::prost::alloc::string::String,
    #[prost(bytes="vec", tag="6")]
    pub data: ::prost::alloc::vec::Vec<u8>,
}

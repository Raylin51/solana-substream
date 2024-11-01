// @generated
// This file is @generated by prost-build.
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct TradeStats {
    #[prost(string, tag="1")]
    pub token_address: ::prost::alloc::string::String,
    #[prost(string, tag="2")]
    pub window: ::prost::alloc::string::String,
    #[prost(int64, tag="3")]
    pub net_buy_volume: i64,
    #[prost(int64, tag="4")]
    pub buy_volume: i64,
    #[prost(int64, tag="5")]
    pub sell_volume: i64,
    #[prost(int64, tag="6")]
    pub total_volume: i64,
    #[prost(int64, tag="7")]
    pub timestamp: i64,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct PumpAlert {
    #[prost(string, tag="1")]
    pub token_address: ::prost::alloc::string::String,
    #[prost(int64, tag="2")]
    pub net_buy_volume_5min: i64,
    #[prost(int64, tag="3")]
    pub total_volume_15min: i64,
    #[prost(int64, tag="4")]
    pub buy_volume_5min: i64,
    #[prost(int64, tag="5")]
    pub sell_volume_5min: i64,
    #[prost(double, tag="6")]
    pub buy_pressure: f64,
    #[prost(int64, tag="7")]
    pub timestamp: i64,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Instructions {
    #[prost(message, repeated, tag="1")]
    pub instructions: ::prost::alloc::vec::Vec<Instruction>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Instruction {
    #[prost(string, tag="1")]
    pub program_id: ::prost::alloc::string::String,
    #[prost(string, repeated, tag="2")]
    pub accounts: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(string, tag="3")]
    pub data: ::prost::alloc::string::String,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Transactions {
    #[prost(message, repeated, tag="1")]
    pub transactions: ::prost::alloc::vec::Vec<Transaction>,
}
#[allow(clippy::derive_partial_eq_without_eq)]
#[derive(Clone, PartialEq, ::prost::Message)]
pub struct Transaction {
    #[prost(string, repeated, tag="1")]
    pub signatures: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(message, repeated, tag="2")]
    pub instructions: ::prost::alloc::vec::Vec<Instruction>,
}
// @@protoc_insertion_point(module)

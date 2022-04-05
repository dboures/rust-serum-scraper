pub mod serum_fill_event_filter;

use {
    serde_derive::Deserialize,
};

#[derive(Clone, Debug, Deserialize)]
pub struct SourceConfig {
    pub dedup_queue_size: usize,
    // pub grpc_sources: Vec<GrpcSourceConfig>,
    // pub snapshot: SnapshotSourceConfig,
    pub rpc_ws_url: String,
}
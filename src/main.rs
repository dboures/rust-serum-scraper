// let's stream the serum event queue (fill events)
// start with a websocket stream, and hopefully build up to a validator plugin

use enumflags2::BitFlags;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{pin_mut, SinkExt, StreamExt};
use log::*;
use serde::Deserialize;
use serum_event_queue_lib::serum_fill_event_filter::deserialize_queue;
use serum_event_queue_lib::serum_fill_event_filter::FillCheckpoint;
use serum_event_queue_lib::serum_fill_event_filter::FillEventFilterMessage;
use serum_event_queue_lib::serum_fill_event_filter::MarketConfig;
use serum_event_queue_lib::serum_fill_event_filter::{self};
use serum_event_queue_lib::websocket_source;
use serum_event_queue_lib::SourceConfig;
use solana_client::rpc_client::RpcClient;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::pin;
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::WebSocketStream;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub source: SourceConfig,
    pub markets: Vec<MarketConfig>,
    pub bind_ws_addr: String,
    pub serum_program_id: String,
}

type CheckpointMap = Arc<Mutex<HashMap<String, FillCheckpoint>>>;
type PeerMap = Arc<Mutex<HashMap<SocketAddr, UnboundedSender<Message>>>>;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        println!("requires a config file argument");
        return Ok(());
    }

    let config: Config = {
        let mut file = File::open(&args[1])?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        toml::from_str(&contents).unwrap()
    };

    solana_logger::setup_with_default("info");
    info!("startup");

    let checkpoints = CheckpointMap::new(Mutex::new(HashMap::new()));
    let peers = PeerMap::new(Mutex::new(HashMap::new()));

    let checkpoints_ref_thread = checkpoints.clone();
    let peers_ref_thread = peers.clone();

    let (account_write_queue_sender, fill_receiver) =
        serum_fill_event_filter::init(config.markets.clone()).await?;

    let rpc_client = RpcClient::new_with_commitment(
        String::from("https://ssc-dao.genesysgo.net/"),
        CommitmentConfig::processed(),
    );

    //let serum_market = Pubkey::from_str("9wFFyRfZBsuAha4YcuxcXLKwMxJR43S7fPfQLusDBzvT")?;
    let q_pubkey = Pubkey::from_str("5KKsLVU6TcbVDK4BS6K1DGDxnh4Q9xjYJ8XaDCG5t8ht")?;

    // filleventfilter websocket sink
    tokio::spawn(async move {
        pin!(fill_receiver);
        loop {
            let message = fill_receiver.recv().await.unwrap();
            match message {
                FillEventFilterMessage::Update(update) => {
                    info!(
                        "MAIN: ws update from fill recvr  {} {:?} fill",
                        update.market, update.status
                    );

                    let mut peer_copy = peers_ref_thread.lock().unwrap().clone();

                    for (k, v) in peer_copy.iter_mut() {
                        debug!("  > {}", k);

                        let json = serde_json::to_string(&update);

                        v.send(Message::Text(json.unwrap())).await.unwrap()
                    }
                }
                FillEventFilterMessage::Checkpoint(checkpoint) => {
                    checkpoints_ref_thread
                        .lock()
                        .unwrap()
                        .insert(checkpoint.queue.clone(), checkpoint);
                }
            }
        }
    });

    // open websocket
    info!("ws listen: {}", config.bind_ws_addr);
    let try_socket = TcpListener::bind(&config.bind_ws_addr).await;
    let listener = try_socket.expect("Failed to bind");
    tokio::spawn(async move {
        // Let's spawn the handling of each connection in a separate task.
        while let Ok((stream, addr)) = listener.accept().await {
            tokio::spawn(handle_connection(peers.clone(), stream, addr));
        }
    });

    info!(
        "rpc connect: {}",
        config
            .source
            .grpc_sources
            .iter()
            .map(|c| c.connection_string.clone())
            .collect::<String>()
    );
    websocket_source::process_events(&config.source, account_write_queue_sender).await;
    Ok(())
}

async fn handle_connection(peer_map: PeerMap, raw_stream: TcpStream, addr: SocketAddr) {
    info!("ws connected: {}", addr);
    let ws_stream = tokio_tungstenite::accept_async(raw_stream)
        .await
        .expect("Error during the ws handshake occurred");
    let (ws_tx, _ws_rx) = ws_stream.split();

    // 1: publish channel in peer map
    let (chan_tx, chan_rx) = unbounded();
    {
        peer_map.lock().unwrap().insert(addr, chan_tx);
        info!("ws published: {}", addr);
    }

    // 2: forward all events from channel to peer socket
    let forward_updates = chan_rx.map(Ok).forward(ws_tx);
    pin_mut!(forward_updates);
    let result_forward = forward_updates.await;

    info!("ws disconnected: {} err: {:?}", &addr, result_forward);
    peer_map.lock().unwrap().remove(&addr);
    result_forward.unwrap();
}

// When connection is made, send them the current event queue
// NEXT: relay updates from changing ev Q account data
// let q_vec: Vec<u8> = rpc_client.get_account_data(&q_pubkey).unwrap();
// send_fills_in_queue(q_vec, peers.clone()).await.unwrap();

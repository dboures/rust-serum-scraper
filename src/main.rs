// let's stream the serum event queue (fill events)
// start with a websocket stream, and hopefully build up to a validator plugin

use arrayref::array_ref;
use enumflags2::BitFlags;
use futures_channel::mpsc::{unbounded, UnboundedSender};
use futures_util::{pin_mut, SinkExt, StreamExt};
use log::*;
use serde::Deserialize;
use serum_dex::state::AccountFlag;
use serum_dex::state::EventView;
use serum_event_queue_lib::serum_fill_event_filter::EventFlag;
use serum_event_queue_lib::serum_fill_event_filter::MarketConfig;
use serum_event_queue_lib::serum_fill_event_filter::SerumEvent;
use serum_event_queue_lib::serum_fill_event_filter::SerumEventQueueHeader;
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
use tokio_tungstenite::tungstenite::protocol::Message;
use tokio_tungstenite::WebSocketStream;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub source: SourceConfig,
    pub markets: Vec<MarketConfig>,
    pub bind_ws_addr: String,
    pub serum_program_id: String,
}

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

    let peers = PeerMap::new(Mutex::new(HashMap::new()));

    let rpc_client = RpcClient::new_with_commitment(
        String::from("https://ssc-dao.genesysgo.net/"),
        CommitmentConfig::processed(),
    );

    //let serum_market = Pubkey::from_str("9wFFyRfZBsuAha4YcuxcXLKwMxJR43S7fPfQLusDBzvT")?;
    let q_pubkey = Pubkey::from_str("5KKsLVU6TcbVDK4BS6K1DGDxnh4Q9xjYJ8XaDCG5t8ht")?;


    // open websocket
    info!("ws listen: {}", config.bind_ws_addr);
    let try_socket = TcpListener::bind(&config.bind_ws_addr).await;
    let listener = try_socket.expect("Failed to bind");
    tokio::spawn(async move {
        // Let's spawn the handling of each connection in a separate task.
        while let Ok((stream, addr)) = listener.accept().await {
            tokio::spawn(handle_connection(peers.clone(), stream, addr));
            
            // When connection is made, send them the current event queue
            // NEXT: relay updates from changing ev Q account data 
            let q_vec: Vec<u8> = rpc_client.get_account_data(&q_pubkey).unwrap();
            send_fills_in_queue(q_vec, peers.clone()).await.unwrap();
        }
    });

    loop {}

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

async fn send_fills_in_queue(queue_data: Vec<u8>, peers_ref_thread: Arc<Mutex<HashMap<SocketAddr, UnboundedSender<Message>>>>) -> anyhow::Result<()> {
    //deserialize the header
    const HEADER_SIZE: usize = 37; //size_of::<SerumEventQueueHeader>();
    const EVENT_SIZE: usize = 88; //size_of::<SerumEvent>();
    const EVENT_LENGTH: usize = 262119; //q_vec.len() - size_of::<SerumEventQueueHeader>();
                                        //TODO: does the header change sizes?

    let header_data = array_ref![queue_data, 0, HEADER_SIZE];
    let event_data = array_ref![queue_data, HEADER_SIZE, EVENT_LENGTH];
    let header: &SerumEventQueueHeader = bytemuck::from_bytes(header_data);
    let flags: BitFlags<AccountFlag> = BitFlags::from_bits(header.account_flags).unwrap();
    assert!(std::str::from_utf8(&header.blob)? == "serum");
    assert!(flags.contains(AccountFlag::EventQueue) && flags.contains(AccountFlag::Initialized));

    let alloc_len = (queue_data.len() - HEADER_SIZE) / EVENT_SIZE;
    let mut start_byte =
        ((header.head + header.count) % u32::try_from(alloc_len)?) * u32::try_from(EVENT_SIZE)?; // start at header

    // go through the whole ring buffer, output fills only
    for i in 0..alloc_len {
        if start_byte + u32::try_from(EVENT_SIZE)? > u32::try_from(EVENT_LENGTH)? {
            start_byte = 0;
        }
        let event_bytes: &[u8; EVENT_SIZE] =
            array_ref![event_data, usize::try_from(start_byte)?, EVENT_SIZE];
        let event: &SerumEvent = bytemuck::from_bytes(event_bytes);
        match event.as_view()? {
            EventView::Fill {
                side: _,
                maker: _,
                native_qty_paid: _,
                native_qty_received: _,
                native_fee_or_rebate: _,
                order_id,
                owner: _,
                owner_slot: _,
                fee_tier: _,
                client_order_id: _,
            } => {
                //send to websocket here
                // info!("side: {:?}, maker: {:?}, native_qty_paid: {:?}, native_qty_received: {:?}, native_fee_or_rebate: {:?},", side, maker, native_qty_paid, native_qty_received, native_fee_or_rebate);
                info!("ws update {:?} fill", order_id);

                let mut peer_copy = peers_ref_thread.lock().unwrap().clone();

                for (k, v) in peer_copy.iter_mut() {
                    debug!("  > {}", k);

                    let json = serde_json::to_string(&order_id);

                    v.send(Message::Text(json.unwrap())).await.unwrap()
                }
                
            }
            EventView::Out { .. } => {}
        };
        start_byte = (start_byte + u32::try_from(EVENT_SIZE)?) % u32::try_from(EVENT_LENGTH)?;
        // info!("start {:?}, seqnum: {:?}", start, header.seq_num + u32::try_from(i)?);
    }
    Ok(())
}

// let's stream the serum event queue (fill events)
// start with a websocket stream, and hopefully build up to a validator plugin

use arrayref::array_ref;
use enumflags2::BitFlags;
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
use std::fs::File;
use std::io::Read;
use std::str::FromStr;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub source: SourceConfig,
    pub markets: Vec<MarketConfig>,
    pub bind_ws_addr: String,
    pub serum_program_id: String,
}

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

    //let serum_program_id = Pubkey::from_str(&config.serum_program_id)?;

    let rpc_client = RpcClient::new_with_commitment(
        String::from("https://ssc-dao.genesysgo.net/"),
        CommitmentConfig::processed(),
    );

    //let serum_market = Pubkey::from_str("9wFFyRfZBsuAha4YcuxcXLKwMxJR43S7fPfQLusDBzvT")?;
    let q_pubkey = Pubkey::from_str("5KKsLVU6TcbVDK4BS6K1DGDxnh4Q9xjYJ8XaDCG5t8ht")?;

    //let account_data: Vec<u8> = rpc_client.get_account_data(&serum_market)?;
    let q_vec: Vec<u8> = rpc_client.get_account_data(&q_pubkey)?;

    info!("{}", q_vec.len());
    deserialize_event_queue(q_vec).unwrap();

    Ok(())
}

fn deserialize_event_queue(queue_data: Vec<u8>) -> anyhow::Result<()> {
    //deserialize the header
    const HEADER_SIZE: usize = 37; //size_of::<SerumEventQueueHeader>();
    const EVENT_SIZE: usize = 88; //size_of::<SerumEvent>();
    const EVENT_LENGTH: usize = 262119; //q_vec.len() - size_of::<SerumEventQueueHeader>();

    let header_data = array_ref![queue_data, 0, HEADER_SIZE];
    let event_data = array_ref![queue_data, HEADER_SIZE, EVENT_LENGTH];
    let header: &SerumEventQueueHeader = bytemuck::from_bytes(header_data);
    // check that blob says Serum
    // check that this is the event queue and it has been initialized
    let flags: BitFlags<AccountFlag> = BitFlags::from_bits(header.account_flags).unwrap();
    info!("header: {:?}", header);
    info!("blob data {:?}", std::str::from_utf8(&header.blob));
    info!("acct_flag {:?}", flags);

    info!("head {:?}", header.head);
    info!("count {:?}", header.count);
    info!("seq_num {:?}", header.seq_num);

    let alloc_len = (queue_data.len() - HEADER_SIZE) / EVENT_SIZE;
    let mut start_byte =
        ((header.head + header.count) % u32::try_from(alloc_len)?) * u32::try_from(EVENT_SIZE)?; // start at header

    info!("queue_data len: {}", queue_data.len());
    info!("HEADER_SIZE: {}", HEADER_SIZE);
    info!("EVENT_SIZE: {}", EVENT_SIZE);
    info!("alloc len: {}", alloc_len);
    info!("start_byte {:?}", start_byte);

    // go through the whole ring buffer, output fills only
    for i in 0..alloc_len {
        if start_byte + u32::try_from(EVENT_SIZE)? > u32::try_from(EVENT_LENGTH)? {
            // TODO: really not sure about this, probably need to wrap around
            continue;
        }
        let event_bytes: &[u8; EVENT_SIZE] =
            array_ref![event_data, usize::try_from(start_byte)?, EVENT_SIZE];
        let event: &SerumEvent = bytemuck::from_bytes(event_bytes);
        match event.as_view()? {
            EventView::Fill {
                side,
                maker,
                native_qty_paid,
                native_qty_received,
                native_fee_or_rebate,
                order_id,
                owner,
                owner_slot,
                fee_tier,
                client_order_id,
            } => {
                info!("side: {:?}, maker: {:?}, native_qty_paid: {:?}, native_qty_received: {:?}, native_fee_or_rebate: {:?},", side, maker, native_qty_paid, native_qty_received, native_fee_or_rebate);
            }
            EventView::Out { .. } => {}
        };
        start_byte = (start_byte + u32::try_from(EVENT_SIZE)?) % u32::try_from(EVENT_LENGTH)?;
        // info!("start {:?}, seqnum: {:?}", start, header.seq_num + u32::try_from(i)?);
    }
    Ok(())
}

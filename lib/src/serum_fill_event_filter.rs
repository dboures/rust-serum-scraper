use solana_sdk::stake_history::Epoch;
use solana_sdk::account::WritableAccount;
use std::collections::HashSet;
use solana_sdk::pubkey::Pubkey;
use std::collections::HashMap;
use crate::{chain_data::ChainData, chain_data::AccountData, chain_data::SlotData, SlotUpdate, AccountWrite};
use serum_dex::matching::Side;
use serum_dex::state::{EventView, QueueHeader};
use bytemuck::Zeroable;
use bytemuck::Pod;
use serum_dex::error::DexResult;
use serde::{Serialize, Deserialize, Serializer, ser::SerializeStruct};
use serum_dex::error::{SourceFileId};
use serum_dex::declare_check_assert_macros;
use enumflags2::BitFlags;
use log::*;

use std::{
    cell::RefMut,
    num::NonZeroU64
};

declare_check_assert_macros!(SourceFileId::State);

#[derive(Copy, Clone, Debug)]
#[repr(packed)]
pub struct SerumEvent {
    pub event_flags: u8,
    pub owner_slot: u8,

    pub fee_tier: u8,

    pub _padding: [u8; 5],

    pub native_qty_released: u64,
    pub native_qty_paid: u64,
    pub native_fee_or_rebate: u64,

    pub order_id: u128,
    pub owner: [u64; 4],
    pub client_order_id: u64,
}
unsafe impl Zeroable for SerumEvent {}
unsafe impl Pod for SerumEvent {}

#[derive(Copy, Clone, BitFlags, Debug)]
#[repr(u8)]
pub enum EventFlag {
    Fill = 0x1,
    Out = 0x2,
    Bid = 0x4,
    Maker = 0x8,
    ReleaseFunds = 0x10,
}

impl EventFlag {
    #[inline]
    fn from_side(side: Side) -> BitFlags<Self> {
        match side {
            Side::Bid => EventFlag::Bid.into(),
            Side::Ask => BitFlags::empty(),
        }
    }

    #[inline]
    fn flags_to_side(flags: BitFlags<Self>) -> Side {
        if flags.contains(EventFlag::Bid) {
            Side::Bid
        } else {
            Side::Ask
        }
    }
}

impl SerumEvent {
    #[inline(always)]
    pub fn as_view(&self) -> DexResult<EventView> {
        let flags = BitFlags::from_bits(self.event_flags).unwrap();
        let side = EventFlag::flags_to_side(flags);
        let client_order_id = NonZeroU64::new(self.client_order_id);
        if flags.contains(EventFlag::Fill) {
            let allowed_flags = {
                use EventFlag::*;
                Fill | Bid | Maker
            };
            check_assert!(allowed_flags.contains(flags))?;

            return Ok(EventView::Fill {
                side,
                maker: flags.contains(EventFlag::Maker),
                native_qty_paid: self.native_qty_paid,
                native_qty_received: self.native_qty_released,
                native_fee_or_rebate: self.native_fee_or_rebate,

                order_id: self.order_id,
                owner: self.owner,

                owner_slot: self.owner_slot,
                fee_tier: self.fee_tier.try_into().or(check_unreachable!())?,
                client_order_id,
            });
        }
        let allowed_flags = {
            use EventFlag::*;
            Out | Bid | ReleaseFunds
        };
        check_assert!(allowed_flags.contains(flags))?;
        Ok(EventView::Out {
            side,
            release_funds: flags.contains(EventFlag::ReleaseFunds),
            native_qty_unlocked: self.native_qty_released,
            native_qty_still_locked: self.native_qty_paid,

            order_id: self.order_id,
            owner: self.owner,

            owner_slot: self.owner_slot,
            client_order_id,
        })
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(packed)]
pub struct SerumEventQueueHeader {
    pub blob: [u8; 5],
    pub account_flags: u64, // Initialized, EventQueue
    pub head: u32,
    pub _padding_one: [u8; 4],
    pub count: u32,
    pub _padding_two: [u8; 4],
    pub seq_num: u32,
    pub _padding_three: [u8; 4],
}


unsafe impl Zeroable for SerumEventQueueHeader {}
unsafe impl Pod for SerumEventQueueHeader {}


pub struct SerumQueue<'a, H: QueueHeader> {
    pub header: RefMut<'a, H>,
    pub buf: RefMut<'a, [H::Item]>,
}

pub type SerumEventQueue<'a> = SerumQueue<'a, SerumEventQueueHeader>;

#[derive(Clone, Debug, Deserialize)]
pub struct MarketConfig {
    pub name: String,
    pub market: String,
    pub event_queue: String,
}


#[derive(Clone, Copy, Debug, Serialize)]
pub enum FillUpdateStatus {
    New,
    Revoke,
}

#[derive(Clone, Debug)]

pub struct FillUpdate {
    pub event: SerumEvent,
    pub status: FillUpdateStatus,
    pub market: String,
    pub queue: String,
}

impl Serialize for FillUpdate {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let event = base64::encode_config(bytemuck::bytes_of(&self.event), base64::STANDARD);
        let mut state = serializer.serialize_struct("FillUpdate", 4)?;
        state.serialize_field("market", &self.market)?;
        state.serialize_field("queue", &self.queue)?;
        state.serialize_field("status", &self.status)?;
        state.serialize_field("event", &event)?;

        state.end()
    }
}

#[derive(Clone, Debug)]
pub struct FillCheckpoint {
    pub market: String,
    pub queue: String,
    pub events: Vec<SerumEvent>,
}

impl Serialize for FillCheckpoint {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let events: Vec<String> = self
            .events
            .iter()
            .map(|e| base64::encode_config(bytemuck::bytes_of(e), base64::STANDARD))
            .collect();
        let mut state = serializer.serialize_struct("FillUpdate", 3)?;
        state.serialize_field("market", &self.market)?;
        state.serialize_field("queue", &self.queue)?;
        state.serialize_field("events", &events)?;
        state.end()
    }
}

pub enum FillEventFilterMessage {
    Update(FillUpdate),
    Checkpoint(FillCheckpoint),
}

type SerumEvents = [SerumEvent; 2900]; // TODO: ah


pub async fn init(
    markets: Vec<MarketConfig>,
) -> anyhow::Result<(
    async_channel::Sender<AccountWrite>,
    async_channel::Sender<SlotUpdate>,
    async_channel::Receiver<FillEventFilterMessage>,
)> {
    // The actual message may want to also contain a retry count, if it self-reinserts on failure?
    let (account_write_queue_sender, account_write_queue_receiver) =
        async_channel::unbounded::<AccountWrite>();

    // Fill updates can be consumed by client connections, they contain all fills for all markets
    let (fill_update_sender, fill_update_receiver) =
        async_channel::unbounded::<FillEventFilterMessage>();

    let account_write_queue_receiver_c = account_write_queue_receiver.clone();

    let mut chain_cache = ChainData::new();
    let mut events_cache: HashMap<String, SerumEvents> = HashMap::new();
    let mut seq_num_cache = HashMap::new();
    let mut last_ev_q_versions = HashMap::<String, (u64, u64)>::new();

    let relevant_pubkeys = markets
        .iter()
        .map(|m| Pubkey::from_str(&m.event_queue).unwrap())
        .collect::<HashSet<Pubkey>>();

    // update handling thread, reads both sloths and account updates
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(account_write) = account_write_queue_receiver_c.recv() => {
                    if !relevant_pubkeys.contains(&account_write.pubkey) {
                        continue;
                    }

                    chain_cache.update_account(
                        account_write.pubkey,
                        AccountData {
                            slot: account_write.slot,
                            write_version: account_write.write_version,
                            account: WritableAccount::create(
                                account_write.lamports,
                                account_write.data.clone(),
                                account_write.owner,
                                account_write.executable,
                                account_write.rent_epoch as Epoch,
                            ),
                        },
                    );
                }
            }

            for mkt in markets.iter() {
                let last_ev_q_version = last_ev_q_versions.get(&mkt.event_queue);
                let mkt_pk = mkt.event_queue.parse::<Pubkey>().unwrap();

                match chain_cache.account(&mkt_pk) {
                    Ok(account_info) => {
                        // only process if the account state changed
                        let ev_q_version = (account_info.slot, account_info.write_version);
                        trace!("evq {} write_version {:?}", mkt.name, ev_q_version);
                        if ev_q_version == *last_ev_q_version.unwrap_or(&(0, 0)) {
                            continue;
                        }
                        last_ev_q_versions.insert(mkt.event_queue.clone(), ev_q_version);

                        let account = &account_info.account;

                        // TODO: deserialize ev Q here
                        // let header, fills = deser()

                        match seq_num_cache.get(&mkt.event_queue) {
                            Some(old_seq_num) => match events_cache.get(&mkt.event_queue) {
                                Some(old_events) => publish_changes(
                                    mkt,
                                    header,
                                    events,
                                    *old_seq_num,
                                    old_events,
                                    &fill_update_sender,
                                ),
                                _ => info!("events_cache could not find {}", mkt.name),
                            },
                            _ => info!("seq_num_cache could not find {}", mkt.name),
                        }

                        seq_num_cache.insert(mkt.event_queue.clone(), header.seq_num.clone());
                        events_cache.insert(mkt.event_queue.clone(), events.clone());
                    }
                    Err(_) => info!("chain_cache could not find {}", mkt.name),
                }
            }
        }
    });

    Ok((
        account_write_queue_sender,
        slot_queue_sender,
        fill_update_receiver,
    ))
}

fn publish_changes(
    mkt: &MarketConfig,
    header: &SerumEventQueueHeader,
    events: &SerumEvents,
    old_seq_num: usize,
    old_events: &SerumEvents,
    fill_update_sender: &async_channel::Sender<FillEventFilterMessage>,
) {
    // seq_num = N means that events (N-QUEUE_LEN) until N-1 are available
    let start_seq_num = max(old_seq_num, header.seq_num) - QUEUE_LEN;
    let mut checkpoint = Vec::new();

    for seq_num in start_seq_num..header.seq_num {
        let idx = seq_num % QUEUE_LEN;

        // there are three possible cases:
        // 1) the event is past the old seq num, hence guaranteed new event
        // 2) the event is not matching the old event queue
        // 3) all other events are matching the old event queue
        // the order of these checks is important so they are exhaustive
        if seq_num >= old_seq_num {
            info!(
                "found new event {} idx {} type {}",
                mkt.name, idx, events[idx].event_type as u32
            );

            // new fills are published and recorded in checkpoint
            if events[idx].event_type == EventType::Fill as u8 {
                let fill: FillEvent = bytemuck::cast(events[idx]);
                fill_update_sender
                    .try_send(FillEventFilterMessage::Update(FillUpdate {
                        event: fill,
                        status: FillUpdateStatus::New,
                        market: mkt.name.clone(),
                        queue: mkt.event_queue.clone(),
                    }))
                    .unwrap(); // TODO: use anyhow to bubble up error
                checkpoint.push(fill);
            }
        } else if old_events[idx].event_type != events[idx].event_type
            || old_events[idx].padding != events[idx].padding
        {
            info!(
                "found changed event {} idx {} seq_num {} header seq num {} old seq num {}",
                mkt.name, idx, seq_num, header.seq_num, old_seq_num
            );

            // first revoke old event if a fill
            if old_events[idx].event_type == EventType::Fill as u8 {
                let fill: FillEvent = bytemuck::cast(old_events[idx]);
                fill_update_sender
                    .try_send(FillEventFilterMessage::Update(FillUpdate {
                        event: fill,
                        status: FillUpdateStatus::Revoke,
                        market: mkt.name.clone(),
                        queue: mkt.event_queue.clone(),
                    }))
                    .unwrap(); // TODO: use anyhow to bubble up error
            }

            // then publish new if its a fill and record in checkpoint
            if events[idx].event_type == EventType::Fill as u8 {
                let fill: FillEvent = bytemuck::cast(events[idx]);
                fill_update_sender
                    .try_send(FillEventFilterMessage::Update(FillUpdate {
                        event: fill,
                        status: FillUpdateStatus::New,
                        market: mkt.name.clone(),
                        queue: mkt.event_queue.clone(),
                    }))
                    .unwrap(); // TODO: use anyhow to bubble up error
                checkpoint.push(fill);
            }
        } else {
            // every already published event is recorded in checkpoint if a fill
            if events[idx].event_type == EventType::Fill as u8 {
                let fill: FillEvent = bytemuck::cast(events[idx]);
                checkpoint.push(fill);
            }
        }
    }

    // in case queue size shrunk due to a fork we need revoke all previous fills
    for seq_num in header.seq_num..old_seq_num {
        let idx = seq_num % QUEUE_LEN;
        info!(
            "found dropped event {} idx {} seq_num {} header seq num {} old seq num {}",
            mkt.name, idx, seq_num, header.seq_num, old_seq_num
        );

        if old_events[idx].event_type == EventType::Fill as u8 {
            let fill: FillEvent = bytemuck::cast(old_events[idx]);
            fill_update_sender
                .try_send(FillEventFilterMessage::Update(FillUpdate {
                    event: fill,
                    status: FillUpdateStatus::Revoke,
                    market: mkt.name.clone(),
                    queue: mkt.event_queue.clone(),
                }))
                .unwrap(); // TODO: use anyhow to bubble up error
        }
    }

    fill_update_sender
        .try_send(FillEventFilterMessage::Checkpoint(FillCheckpoint {
            events: checkpoint,
            market: mkt.name.clone(),
            queue: mkt.event_queue.clone(),
        }))
        .unwrap()
}
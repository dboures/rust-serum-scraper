use serum_dex::matching::Side;
use serum_dex::state::{EventView, QueueHeader};
use bytemuck::Zeroable;
use bytemuck::Pod;
use serum_dex::error::DexResult;
use serde::{Deserialize};
use serum_dex::error::{SourceFileId};
use serum_dex::declare_check_assert_macros;
use enumflags2::BitFlags;

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

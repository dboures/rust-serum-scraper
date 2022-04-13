use crate::serum_fill_event_filter::deserialize_queue;
use std::string::String;
use jsonrpc_core::futures::StreamExt;
use jsonrpc_core_client::transports::{http, ws};

use solana_account_decoder::{UiAccountEncoding, UiAccount};
use solana_client::{
    rpc_config::{RpcAccountInfoConfig, RpcProgramAccountsConfig},
    //rpc_filter::RpcFilterType,
    rpc_response::{Response, RpcKeyedAccount},
};
use solana_rpc::{
    rpc::rpc_accounts::AccountsDataClient, rpc::OptionalContext, rpc_pubsub::RpcSolPubSubClient,
};
use solana_sdk::{account::Account, commitment_config::CommitmentConfig, pubkey::Pubkey};

use log::*;
use std::{
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{AccountWrite, AnyhowWrap, SlotStatus, SlotUpdate, SourceConfig};

enum WebsocketMessage {
    SingleUpdate(Response<RpcKeyedAccount>),
    SnapshotUpdate(Response<Vec<RpcKeyedAccount>>),
    SlotUpdate(Arc<solana_client::rpc_response::SlotUpdate>),
}

// TODO: the reconnecting should be part of this
async fn feed_data(
    config: &SourceConfig,
    sender: async_channel::Sender<WebsocketMessage>,
) -> anyhow::Result<()> {
    let program_id = Pubkey::from_str(&config.snapshot.program_id)?;
    let snapshot_duration = Duration::from_secs(300);

    let connect = ws::try_connect::<RpcSolPubSubClient>(&config.rpc_ws_url).map_err_anyhow()?;
    let client = connect.await.map_err_anyhow()?;

    let rpc_client =
        http::connect::<AccountsDataClient>(&config.snapshot.rpc_http_url)
            .await
            .map_err_anyhow()?;

    let account_info_config = RpcAccountInfoConfig {
        encoding: Some(UiAccountEncoding::Base64),
        commitment: Some(CommitmentConfig::processed()),
        data_slice: None,
    };
    let program_accounts_config = RpcProgramAccountsConfig {
        filters: None,
        with_context: Some(true),
        account_config: account_info_config.clone(),
    };

    //let mut update_sub = client
        // .program_subscribe(
        //     program_id.to_string(),
        //     Some(program_accounts_config.clone()),
        // )
        // .map_err_anyhow()?;
    //let mut slot_sub = client.slots_updates_subscribe().map_err_anyhow()?;

    let mut account_sub = client.account_subscribe(String::from("5KKsLVU6TcbVDK4BS6K1DGDxnh4Q9xjYJ8XaDCG5t8ht"), Some(account_info_config)).map_err_anyhow()?;

    let mut last_snapshot = Instant::now() - snapshot_duration;

    loop {
        tokio::select! {
            account = account_sub.next() => {
                match account {
                    Some(account) => {

                        info!("account update");
                        let response = account.unwrap();
                        let new_resp = Response {
                            context: response.context,
                            value: RpcKeyedAccount {
                                pubkey: String::from("5KKsLVU6TcbVDK4BS6K1DGDxnh4Q9xjYJ8XaDCG5t8ht"),
                                account: response.value,
                            }
                        };
                        sender.send(WebsocketMessage::SingleUpdate(new_resp)).await.expect("sending must succeed");
                    },
                    None => {
                        warn!("account stream closed");
                        return Ok(());
                    },
                }
            },
            _ = (tokio::time::sleep(Duration::from_millis(1000))) => {
                info!("pass");
                return Ok(())
            }
        }
    }
}

// TODO: rename / split / rework
pub async fn process_events(
    config: &SourceConfig,
    account_write_queue_sender: async_channel::Sender<AccountWrite>,
) {
    // Subscribe to program account updates websocket
    let (update_sender, update_receiver) = async_channel::unbounded::<WebsocketMessage>();
    let config = config.clone();
    tokio::spawn(async move {
        // if the websocket disconnects, we get no data in a while etc, reconnect and try again
        loop {
            let out = feed_data(&config, update_sender.clone());
            let _ = out.await;
        }
    });

    //
    // The thread that pulls updates and forwards them to postgres
    //

    // copy websocket updates into the postgres account write queue
    loop {
        let update = update_receiver.recv().await.unwrap();
        info!("got update message");

        match update {
            WebsocketMessage::SingleUpdate(update) => {
                info!("single update");
                let account: Account = update.value.account.decode().unwrap();
                let pubkey = Pubkey::from_str(&update.value.pubkey).unwrap();

                let (_header, fills) = deserialize_queue(account.data.to_vec()).unwrap();

                info!("send account write {:?}", fills);

                account_write_queue_sender
                    .send(AccountWrite::from(pubkey, update.context.slot, 0, account))
                    .await
                    .expect("send success");
            }
            WebsocketMessage::SnapshotUpdate(update) => {
                info!("snapshot update");
            }
            WebsocketMessage::SlotUpdate(update) => {
                info!("sloot update")
            }
        }
    }
}

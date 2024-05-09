use std::time::Duration;

use bevy::prelude::*;
use futures::StreamExt;
use nostr_sdk::{
    serde_json, Client, ClientMessage, Event as NostrEvent, EventBuilder, Filter, Kind,
    RelayPoolNotification, Tag, Timestamp,
};

use wasm_bindgen_futures::spawn_local;
use web_sys::window;

use crate::{
    components::CoinMove,
    messages::{NetworkMessage, Players},
    resources::{Board, GameState, NetworkStuff, PlayerMove},
    AppState,
};

const COIN_SIZE: Vec2 = Vec2::new(40.0, 40.0);
const COLUMNS: usize = 7;
const ROWS: usize = 7;
const SPACING: f32 = 5.0;

pub struct NostrPlugin;

impl Plugin for NostrPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(NetworkStuff::new())
            .insert_resource(GameState::new())
            .add_systems(OnEnter(AppState::InGame), setup)
            .add_systems(Update, handle_net_msg.run_if(in_state(AppState::InGame)));
    }
}

fn setup(mut network_stuff: ResMut<NetworkStuff>, mut game_state: ResMut<GameState>) {
    let window = window().expect("no global `window` exists");
    let local_storage = window
        .local_storage()
        .expect("no local storage")
        .expect("local storage is not available");

    if let Ok(Some(username)) = local_storage.get_item("username") {
        game_state.local_ln_address = Some(username.clone());
        info!("username found in local storage {:?}", username)
    } else {
        info!("no username found in local storage")
    }

    let (send_tx, send_rx) = futures::channel::mpsc::channel::<String>(1000);
    let (nostr_msg_tx, mut nostr_msg_rx) = futures::channel::mpsc::channel::<ClientMessage>(1000);

    let nostr_msg_tx_clone = nostr_msg_tx.clone();

    let location = web_sys::window().unwrap().location();
    let game_id = location.pathname().unwrap().to_string();
    let tag = format!("unite4.luvnft.com game_id = {}", game_id);
    game_state.game_tag = Tag::Hashtag(tag.clone());

    let game_state_clone = game_state.clone();
    let game_state_clone_2 = game_state.clone();

    network_stuff.read = Some(send_rx);
    game_state.send = Some(nostr_msg_tx);

    spawn_local(async move {
        let nostr_keys = &game_state_clone.nostr_keys;
        let client = Client::new(nostr_keys);

        let relays: String = if let Ok(Some(relays)) = local_storage.get_item("Relays") {
            info!("relays found in local storage {:?}", relays);
            relays
        } else {
            info!("no relays found in local storage");
            "".to_string()
        };

        let relay_urls: Vec<&str> = relays.split(',').collect();

        for relays in relay_urls {
            match client.add_relay(relays).await {
                Ok(_) => {
                    info!("relay added: {:?}", relays);
                }
                Err(e) => {
                    error!("error adding relay: {:?}", e);
                }
            };
        }

        client.connect().await;

        let client_clone = client.clone();

        spawn_local(async move {
            while let Some(msg) = nostr_msg_rx.next().await {
                info!("sent event: {:?}", msg);
                match client_clone.clone().send_msg(msg).await {
                    Ok(_) => {}
                    Err(e) => {
                        let window = web_sys::window().unwrap();
                        if let Some(window) = Some(window) {
                            let alert_message = format!("Error connecting to nostr: {:?}", e);
                            match window.alert_with_message(&alert_message) {
                                Ok(_) => {}
                                Err(js_err) => {
                                    info!("Error sending alert: {:?}", js_err)
                                }
                            }
                        }
                        error!("Error sending message: {:?}", e);
                    }
                };
            }
        });

        let filter = Filter::new().kind(Kind::Regular(4444)).hashtag(tag.clone());

        client.subscribe(vec![filter.clone()]).await;

        let mut events: Vec<NostrEvent> = client
            .get_events_of(vec![filter], Some(Duration::new(10, 0)))
            .await
            .unwrap();

        events.reverse();

        info!("nostr_key: {:?}", nostr_keys.public_key());

        if let Some(last_event) = events.last() {
            match serde_json::from_str::<NetworkMessage>(&last_event.content) {
                Ok(NetworkMessage::NewGame(player)) => {
                    info!("current tip: {:?}", last_event.content);
                    if last_event.pubkey != nostr_keys.public_key() {
                        let players = if game_state_clone_2.local_ln_address.is_none() {
                            Players::new(
                                player,
                                None,
                                last_event.pubkey.clone(),
                                nostr_keys.public_key(),
                            )
                        } else {
                            Players::new(
                                player,
                                game_state_clone_2.local_ln_address.clone(),
                                last_event.pubkey.clone(),
                                nostr_keys.public_key(),
                            )
                        };

                        let msg = NetworkMessage::JoinGame(players);
                        let serialized_message = serde_json::to_string(&msg).unwrap();

                        let nostr_msg = ClientMessage::event(
                            EventBuilder::new(
                                Kind::Regular(4444),
                                serialized_message,
                                [Tag::Hashtag(tag.clone())],
                            )
                            .to_event(nostr_keys)
                            .unwrap(),
                        );

                        match nostr_msg_tx_clone.clone().try_send(nostr_msg) {
                            Ok(()) => {}
                            Err(e) => {
                                error!("Error sending join_game message: {}", e)
                            }
                        };
                    } else {
                        info!("skipping own new game event");
                    }
                }
                _ => {
                    info!("current tip: {:?}", last_event.content);
                }
            }
        } else {
            info!("current tip: no events");
            let msg = if game_state_clone_2.local_ln_address.is_none() {
                NetworkMessage::NewGame(None)
            } else {
                NetworkMessage::NewGame(game_state_clone_2.local_ln_address.clone())
            };

            let serialized_message = serde_json::to_string(&msg).unwrap();

            let nostr_msg = ClientMessage::event(
                EventBuilder::new(
                    Kind::Regular(4444),
                    serialized_message,
                    [Tag::Hashtag(tag.clone())],
                )
                .to_event(nostr_keys)
                .unwrap(),
            );

            match nostr_msg_tx_clone.clone().try_send(nostr_msg) {
                Ok(()) => {}
                Err(e) => {
                    error!("Error sending join_game message: {}", e)
                }
            };
        };

        for event in events.drain(..) {
            if (event.content.contains("NewGame") || event.content.contains("JoinGame"))
                && event.pubkey == nostr_keys.public_key()
            {
                info!("skipping event");
                continue;
            }
            if event.content.contains("NewGame") {
                //this means you are player 2 so you only sub to p1 events
                let new_subscription = Filter::new()
                    .author(event.pubkey)
                    .kind(Kind::Regular(4444))
                    .since(Timestamp::now())
                    .hashtag(tag.clone());

                info!("sub to player 1 events only {:?}", event.pubkey);

                client.subscribe(vec![new_subscription]).await;
            }
            //this means you are player 1 so you only sub to p2 events
            if event.content.contains("JoinGame") {
                let new_subscription = Filter::new()
                    .author(event.pubkey)
                    .kind(Kind::Regular(4444))
                    .since(Timestamp::now())
                    .hashtag(tag.clone());

                info!("sub to player 2 events only {:?}", event.pubkey);

                client.subscribe(vec![new_subscription]).await;
            }

            info!("processing stored event: {:?}", event);

            match send_tx.clone().try_send(event.content.clone()) {
                Ok(()) => {}
                Err(e) => {
                    error!("Error sending message: {} CHANNEL FULL???", e)
                }
            };
        }

        client
            .handle_notifications(|notification| async {
                if let RelayPoolNotification::Event {
                    relay_url: _,
                    event,
                } = notification
                {
                    if event.pubkey != nostr_keys.public_key() {
                        info!("received event: {:?}", event);
                        if event.content.contains("JoinGame") {
                            let new_subscription = Filter::new()
                                .author(event.pubkey)
                                .kind(Kind::Regular(4444))
                                .since(Timestamp::now())
                                .hashtag(tag.clone());

                            info!("sub to player 2 events only {:?}", event.pubkey);

                            client.subscribe(vec![new_subscription]).await;
                        }

                        match send_tx.clone().try_send(event.content.clone()) {
                            Ok(()) => {}
                            Err(e) => {
                                error!("Error sending message: {} CHANNEL FULL???", e)
                            }
                        };
                    }
                }

                Ok(false)
            })
            .await
            .unwrap();
    });
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_net_msg(
    mut network_stuff: ResMut<NetworkStuff>,
    mut game_state: ResMut<GameState>,
    mut board: ResMut<Board>,
    mut commands: Commands,
    asset_server: Res<AssetServer>,
) {
    if let Some(ref mut receive_rx) = network_stuff.read {
        while let Ok(Some(message)) = receive_rx.try_next() {
            match serde_json::from_str::<NetworkMessage>(&message) {
                Ok(network_message) => match network_message {
                    NetworkMessage::Input(new_input) => {
                        let row_pos = board.moves.iter().filter(|m| m.column == new_input).count();
                        if row_pos <= 5 {
                            let player_move =
                                PlayerMove::new(board.player_turn, new_input, row_pos);

                            board.moves.push(player_move);

                            let offset_x = -COIN_SIZE.x * (COLUMNS as f32) / 2.0;
                            let offset_y = -COIN_SIZE.y * (ROWS as f32) / 2.0;

                            if board.player_turn == 1 {
                                commands
                                    .spawn(SpriteBundle {
                                        sprite: Sprite {
                                            custom_size: Some(COIN_SIZE),
                                            ..Default::default()
                                        },
                                        texture: asset_server.load("red_circle.png"),
                                        transform: Transform::from_xyz(
                                            offset_x + new_input as f32 * (COIN_SIZE.x + SPACING),
                                            offset_y + 6_f32 * (COIN_SIZE.y + SPACING),
                                            1.0,
                                        ),
                                        ..Default::default()
                                    })
                                    .insert(CoinMove::new(player_move));
                            } else {
                                commands
                                    .spawn(SpriteBundle {
                                        sprite: Sprite {
                                            custom_size: Some(COIN_SIZE),
                                            ..Default::default()
                                        },
                                        texture: asset_server.load("yellow_circle.png"),
                                        transform: Transform::from_xyz(
                                            offset_x + new_input as f32 * (COIN_SIZE.x + SPACING),
                                            offset_y + 6_f32 * (COIN_SIZE.y + SPACING),
                                            1.0,
                                        ),
                                        ..Default::default()
                                    })
                                    .insert(CoinMove::new(player_move));
                            }

                            board.player_turn = if board.player_turn == 1 { 2 } else { 1 };

                            break;
                        }
                    }
                    NetworkMessage::JoinGame(players) => {
                        if game_state.nostr_keys.public_key() != players.p1_pubkey
                            && game_state.nostr_keys.public_key() != players.p2_pubkey
                        {
                            info!("not your game {:?}", players);
                            game_state.player_type = 3;
                            continue;
                        }

                        if game_state.start {
                            continue;
                        }

                        game_state.p2_ln_address = players.p2_name;

                        game_state.player_type = 1;
                        info!("player type: 1");
                        game_state.start = true;
                    }
                    NetworkMessage::NewGame(player1) => {
                        if game_state.start {
                            continue;
                        }

                        game_state.p2_ln_address = player1;
                        //recevied message from p1 so you must be p2
                        game_state.player_type = 2;
                        info!("player type: 2");
                        game_state.start = true;
                    }
                },

                Err(e) => {
                    info!("Failed to deserialize message: {:?}", e);
                }
            }
        }
    }
}

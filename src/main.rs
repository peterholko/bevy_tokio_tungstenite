// Configure clippy for Bevy usage
#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::enum_glob_use)]

use bevy::{
    app::{ScheduleRunnerPlugin, ScheduleRunnerSettings},
    core::CorePlugin,
    prelude::*,
    tasks::IoTaskPool,
    utils::Duration,
};
use crossbeam_channel::{unbounded, Receiver as CBReceiver, Sender as CBSender};
use tokio::sync::mpsc::Sender;

use futures_util::{SinkExt, StreamExt};
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::{Message, Result};
use tokio_tungstenite::{accept_async, tungstenite::Error};

use async_compat::Compat;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

const TIMESTEP_5_PER_SECOND: f64 = 30.0 / 60.0;

#[derive(Debug, Clone)]
pub struct Client {
    pub id: i32,
    pub sender: Sender<String>,
}

type Clients = Arc<Mutex<HashMap<i32, Client>>>;

fn main() {
    App::build()
        .insert_resource(ScheduleRunnerSettings::run_loop(Duration::from_secs_f64(
            TIMESTEP_5_PER_SECOND,
        )))
        .add_plugin(CorePlugin::default())
        .add_plugin(ScheduleRunnerPlugin::default())
        .add_startup_system(setup.system())
        .add_system(message_system.system())
        .run();
}

fn setup(mut commands: Commands, task_pool: Res<IoTaskPool>) {
    println!("Bevy Setup System");

    //Initialize Arc Mutex Hashmap to store the client to game channel per connected client
    let clients: Clients = Arc::new(Mutex::new(HashMap::new()));

    //Create the client to game channel, note the sender will be cloned by each connected client
    let (client_to_game_sender, client_to_game_receiver) = unbounded::<String>();

    //Spawn the tokio runtime setup using a Compat with the clients and client to game channel
    task_pool
        .spawn(Compat::new(tokio_setup(
            clients.clone(),
            client_to_game_sender,
        )))
        .detach();

    //Insert the clients and client to game channel into the Bevy resources
    commands.insert_resource(clients);
    commands.insert_resource(client_to_game_receiver);
}

async fn tokio_setup(clients: Clients, client_to_game_sender: CBSender<String>) {
    env_logger::init();

    let addr = "127.0.0.1:9002";
    let listener = TcpListener::bind(&addr).await.expect("Can't listen");
    println!("Listening on: {}", addr);

    while let Ok((stream, _)) = listener.accept().await {
        let peer = stream
            .peer_addr()
            .expect("connected streams should have a peer address");
        println!("Peer address: {}", peer);

        //Spawn a connection handler per client
        tokio::spawn(accept_connection(
            peer,
            stream,
            clients.clone(),
            client_to_game_sender.clone(),
        ));
    }

    println!("Finished");
}

async fn accept_connection(
    peer: SocketAddr,
    stream: TcpStream,
    clients: Clients,
    client_to_game_sender: CBSender<String>,
) {
    if let Err(e) = handle_connection(peer, stream, clients, client_to_game_sender).await {
        match e {
            Error::ConnectionClosed | Error::Protocol(_) | Error::Utf8 => (),
            err => println!("Error processing connection: {}", err),
        }
    }
}

async fn handle_connection(
    peer: SocketAddr,
    stream: TcpStream,
    clients: Clients,
    client_to_game_sender: CBSender<String>,
) -> Result<()> {
    println!("New WebSocket connection: {}", peer);
    let ws_stream = accept_async(stream).await.expect("Failed to accept");

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    //Create a tokio sync channel to for messages from the game to each client
    let (game_to_client_sender, mut game_to_client_receiver) = tokio::sync::mpsc::channel(100);

    //Get the number of clients for a client id
    let num_clients = clients.lock().unwrap().keys().len() as i32;

    //Store the incremented client id and the game to client sender in the clients hashmap
    clients.lock().unwrap().insert(
        num_clients + 1,
        Client {
            id: num_clients + 1,
            sender: game_to_client_sender,
        },
    );

    //This loop uses the tokio select! macro to receive messages from either the websocket receiver
    //or the game to client receiver
    loop {
        tokio::select! {
            //Receive messages from the websocket
            msg = ws_receiver.next() => {
                match msg {
                    Some(msg) => {
                        let msg = msg?;
                        if msg.is_text() ||msg.is_binary() {
                            client_to_game_sender.send(msg.to_string()).expect("Could not send message");
                        } else if msg.is_close() {
                            break;
                        }
                    }
                    None => break,
                }
            }
            //Receive messages from the game
            game_msg = game_to_client_receiver.recv() => {
                let game_msg = game_msg.unwrap();
                ws_sender.send(Message::Text(game_msg)).await?;
            }

        }
    }

    Ok(())
}

fn message_system(clients: Res<Clients>, client_to_game_receiver: Res<CBReceiver<String>>) {
    //Broadcast a message to each connected client on each Bevy System iteration.
    for (_id, client) in clients.lock().unwrap().iter() {
        println!("{:?}", client);
        client
            .sender
            .try_send("Broadcast message from Bevy System".to_string())
            .expect("Could not send message");
    }

    //Attempts to receive a message from the channel without blocking.
    if let Ok(msg) = client_to_game_receiver.try_recv() {
        println!("{:?}", msg);
    }
}

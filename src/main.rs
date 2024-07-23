use std::collections::HashMap;
use std::default::Default;
use std::{fs, process};
use std::io::{self,Write,stdout};
use std::panic::{self,PanicInfo};
use std::process::{Stdio,Command};
use std::env;

use a2s::{self, A2SClient};


use serde::{Deserialize, Serialize};

use serenity::all::{ActivityData, Context, EventHandler, GatewayIntents, Ready};
use serenity::async_trait;
use serenity::prelude::TypeMapKey;
use serenity::utils::token::validate;
use serenity::Client;

use toml::{Table, Value};

use tokio::task::JoinSet;
use tokio::time::{sleep, Duration};
use tokio::{select, signal};

use thiserror::Error;

use crossterm::style::{Colors,Color,SetColors};
use crossterm::ExecutableCommand;
use crossterm::terminal::SetTitle;

#[derive(Serialize, Deserialize, Debug)]
#[serde(default)]
#[derive(Clone)]
struct Server {
    enable: bool,
    address: String,
    apiKey: String,
}

impl Default for Server {
    fn default() -> Self {
        Server {
            enable: true,
            address: "localhost:8000".to_string(),
            apiKey: String::new(),
        }
    }
}

//TODO cant get defaults to work with newtype pattern ie. #[serde(transparent)] with #[serde(default)]
#[derive(Serialize, Deserialize, Debug)]
struct ConfigLayout {
    refreshInterval: String,
    #[serde(flatten)] //gets rid of tables name
    servers: HashMap<String, Server>,
}

impl Default for ConfigLayout {
    fn default() -> Self {
        let mut map = HashMap::<String, Server>::new();
        map.insert("example-server".into(), Server::default());
        ConfigLayout {
            refreshInterval: "30s".into(),
            servers: map,
        }
    }
}

struct Handler;
#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        tokio::spawn(server_activity(ctx));
    }
}

//changes bots activity to show player count
async fn server_activity(ctx: Context) -> () {
    loop {
        let guard = ctx.data.read().await;
        let addr = guard.get::<TMAddress>().unwrap();
        let a2s = A2SClient::new().await.unwrap();
        let status: String;
        match a2s.info(addr).await {
            Ok(info) => {
                status = format!("Playing {}/{}", info.players, info.max_players);
            }
            Err(_) => {
                status = "Offline".into();
            }
        }
        ctx.set_activity(Some(ActivityData::custom(status)));
        sleep(Duration::from_secs(30)).await;
    }
}

//insert the server address into the context data of event handler
struct TMAddress(String);
impl TypeMapKey for TMAddress {
    type Value = String;
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid discord api key '{0}'")]
    InvalidToken(String),
}

async fn watch_server(name: String, server: Server) -> anyhow::Result<()> {
    validate(&server.apiKey).map_err(|_| Error::InvalidToken(server.apiKey.clone()))?;
    let mut client = Client::builder(&server.apiKey, GatewayIntents::default())
        .event_handler(Handler)
        .await?;
    client
        .data
        .write()
        .await
        .insert::<TMAddress>(server.address);
    loop {
        match client.start().await {
            Ok(_) => {}
            Err(err) => {
                println!("Server {} crashed: {}. (Attempting restart)", name, err);
            }
        }
    }
}

//keeps the terminal window open after quitting
//wont stay open if any of this panics aswell
fn quit(msg: &PanicInfo<'_>) -> () {
    println!("{}",msg);
    println!("Press Enter to exit...");
    io::stdout().flush().expect("Failed to flush stdout");
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .expect("Failed to read line");
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> () {


    //boostraps the program onto conhost (using -b arg) to customise the window size
    #[cfg(target_os = "windows")] {
        let args: Vec<String> = env::args().collect();
        if !args.iter().any(|i| i=="-b"){
            Command::new("conhost")
            .args(["cmd.exe" ,"/K","mode con cols=50 lines=3 && player-count-discord-bot.exe -b"])
            .spawn()
            .expect("failed to boostrap onto conhost.");
            return;
        } //else actually run the program
    }

    panic::set_hook(Box::new(&quit));
    stdout().execute(SetTitle("Player Count Discord Bot")).unwrap();
    stdout().execute(SetColors(Colors::new(Color::DarkGreen,Color::Black))).unwrap();

    let config_path = "./config.toml".to_string();

    //create config file if doesnt exist
    if fs::metadata(&config_path).is_err() {
        fs::File::create_new(&config_path).unwrap();
    }
    let toml = fs::read_to_string("./config.toml").unwrap();

    let config_doc: Table = toml::from_str(&toml).expect("config file doesn't contain valid TOML");

    let mut config = ConfigLayout::default();

    //if toml doc empty, fill with default, and write back to file
    //gotta be a cleaner way to do this... however if i deserialise directly to ConfigLayout,
    //then will crash when there is a unkown global key in the config file, due to serde(flatten) - will try to consume
    //it, and cant parse it as its the wrong toml::Value type
    if config_doc.len() == 0 {
        fs::write(&config_path, toml::to_string(&config).unwrap().as_str()).unwrap();
    } else {
        config.servers.drain(); //dont need the example server
        //deserialise toml
        for (name, value) in config_doc {
            if name == "refreshInterval" {
                if let Value::String(v) = &value {
                    config.refreshInterval = value.to_string();
                }
            } else if let Value::Table(v) = &value {
                let s = value.try_into::<Server>().unwrap();
                config.servers.insert(name, s);
            }
        }
    }

    let mut tasks = JoinSet::new();
    for (name, server) in &config.servers {
        //spawn jobs for each server bot
        if server.enable {
            tasks.spawn(watch_server(name.clone(), server.clone()));
        }
    }

    while !tasks.is_empty() {
        select! {
            _ = signal::ctrl_c() => {
                tasks.abort_all();
            },
            Some(r) = tasks.join_next() => {
                match r {
                    Ok(r) => {
                        if let Err(e) = r {
                            println!("task error: {}",e);
                        }
                    }
                    Err(_) => {//already prints the panic in quit() hook
                    }
                }
            }
        };
    }
    //TODO hot reload if config file changes
}

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
use tokio::time::{sleep,Duration};
use tokio::{select, signal};

use thiserror::Error;

use crossterm::style::{Colors,Color,SetColors};
use crossterm::ExecutableCommand;
use crossterm::terminal::{SetSize, SetTitle};

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
    #[serde(with = "humantime_serde")]
    refreshInterval: Duration,
    #[serde(flatten)] //gets rid of tables name
    servers: HashMap<String, Server>,
}

impl Default for ConfigLayout {
    fn default() -> Self {
        let mut map = HashMap::<String, Server>::new();
        map.insert("example-server".into(), Server::default());
        ConfigLayout {
            refreshInterval: Duration::new(30, 0),
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
        let refresh_interval = guard.get::<TMRefreshInterval>().unwrap();
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
        sleep(refresh_interval.clone()).await;
    }
}

//this is to insert data into the context of event handler
struct TMAddress;
impl TypeMapKey for TMAddress {
    type Value = String;
}

struct TMRefreshInterval;
impl TypeMapKey for TMRefreshInterval {
    type Value = Duration;
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid discord api key '{0}'")]
    InvalidToken(String),
}

async fn watch_server(name: String, server: Server, refresh_interval : Duration) -> anyhow::Result<()> {
    validate(&server.apiKey).map_err(|_| Error::InvalidToken(server.apiKey.clone()))?;
    let mut client = Client::builder(&server.apiKey, GatewayIntents::default()).event_handler(Handler).await?;
    client.data.write().await.insert::<TMAddress>(server.address);
    client.data.write().await.insert::<TMRefreshInterval>(refresh_interval);
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
fn quit() -> () {
    println!("Press Enter to exit...");
    io::stdout().flush().expect("Failed to flush stdout");
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .expect("Failed to read line");
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> () {


    //boostrap the program onto conhost (using -b arg) to customise the window size
    #[cfg(target_os = "windows")] {
        let args: Vec<String> = env::args().collect();
        if !args.iter().any(|i| i=="-b"){
            Command::new("conhost")
            .args(["cmd.exe" ,"/K","mode con cols=50 lines=10 && player-count-discord-bot.exe -b && exit"])
            .spawn()
            .expect("failed to boostrap onto conhost.");
            return;
        } //else actually run the program
    }

    panic::set_hook(Box::new(|msg: &PanicInfo<'_>| {
        println!("{}",msg);
        quit();
    }));
    stdout().execute(SetTitle("Player Count Bots")).unwrap();
    stdout().execute(SetColors(Colors::new(Color::DarkGreen,Color::Black))).unwrap();
    stdout().execute(SetSize(50,100)).unwrap(); //make screen buffer larger than the window, so can see print history.
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
                    config.refreshInterval = v.parse::<humantime::Duration>().unwrap().into();
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
            tasks.spawn(watch_server(name.clone(), server.clone(), config.refreshInterval.clone()));
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
    quit();
    //TODO hot reload if config file changes
}

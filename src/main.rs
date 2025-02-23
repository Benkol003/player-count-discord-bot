use std::collections::HashMap;
use std::default::Default;
use std::sync::Arc;
use std::{fs};
use std::io::{self,Write,stdout};
use std::panic::{self, PanicHookInfo};
use std::process::{Command};
use std::env;

use simplelog::*;
use log::LevelFilter;

use log::{info,error,debug,warn};

use a2s::{self, A2SClient};


use serde::{Deserialize, Serialize};

use serenity::all::{ActivityData, ChannelOverwriteAction, ConnectionStage, Context, EventHandler, GatewayIntents, Ready, ShardId, ShardManager, ShardRunner, ShardStageUpdateEvent};
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
        warn!("{} reconnected", ready.user.name);
    }

    async fn shard_stage_update(&self, ctx: Context, shard_update: ShardStageUpdateEvent) {
        warn!("{:?}",shard_update);
    }
}

//changes bots activity to show player count
async fn server_activity(server: Server,refresh_interval: Duration, shard_manager: Arc<ShardManager>) -> () {
    let mut shard_restart = false;
    loop {
        if(shard_restart){
            warn!("[{}] restarting shard as in connecting stage, (typically gets stuck trying to reconnect)",&server.address);
            shard_manager.shutdown(ShardId(0),0).await;
            match shard_manager.initialize() {
                Ok(_) => {
                    warn!("[{}] restarted shard",&server.address);
                }
                Err(err) => {
                    warn!("[{}] failed to restart shard: {}",&server.address,err);
                }
            }
            shard_restart=false;
        } else {
            let shards_lock = shard_manager.runners.lock().await;
            match shards_lock.get(&ShardId(0)) {
                None => {
                    warn!("{}: could not find shard with ID 0",&server.address);
                }
                Some(shard_runner) => {
                    //TODO there has to be a better way to check if the shard reciever is gone, maybe check heartbeat
                    if shard_runner.stage.is_connecting() {
                        //shutting down shard will remove it from shardManager. If we call this with a lock on the shard runner then we get a deadlock.
                        shard_restart=true; continue;
                    }
    
                    let a2s = A2SClient::new().await.unwrap();
                    let status: String;
                    match a2s.info(&server.address).await {
                        Ok(info) => {
                            status = format!("Playing {}/{}", info.players, info.max_players);
                        }
                        Err(_) => {
                            status = "Offline".into();
                        }
                    }
                    shard_runner.runner_tx.set_activity(Some(ActivityData::custom(status)));
                }
            }
        }
        //dont sleep whilst holding the lock...
        sleep(refresh_interval).await;
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid discord api key '{0}'")]
    InvalidToken(String),
}

async fn watch_server(name: String, server: Server, refresh_interval : Duration) -> anyhow::Result<()> {
    validate(&server.apiKey).map_err(|_| Error::InvalidToken(server.apiKey.clone()))?;
    let mut client = Client::builder(&server.apiKey, GatewayIntents::default()).event_handler(Handler).await?;

    let shard_manager = client.shard_manager.clone();

    let _ = tokio::spawn(async move{
        loop {
            match client.start().await {
                Ok(_) => {
                    println!("started.");
                }
                Err(err) => {
                    error!("Server {} crashed: {}. (Attempting restart)", name.clone(), err);
                }
            }
        }
    });

    server_activity(server,refresh_interval,shard_manager).await;
    warn!("server activity task stopped");
    return Ok(());
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

    WriteLogger::init(LevelFilter::Warn, Config::default(),fs::OpenOptions::new().create(true).write(true).append(true).open("log.txt").unwrap()).unwrap();

    warn!("startup");

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

    panic::set_hook(Box::new(|msg: &PanicHookInfo<'_>| {
        error!("panic: {msg}\n");
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
            println!("Running: [{}]",&name);
        }
    }

    while !tasks.is_empty() {
        select! {
            _ = signal::ctrl_c() => {
                tasks.abort_all();
                warn!("CTRL_C recieved, shutdown")
            },
            Some(r) = tasks.join_next() => {
                match r {
                    Ok(r) => {
                        if let Err(e) = r {
                            error!("task error: {}\n",e);
                        }
                    }
                    Err(e) => {//already prints the panic in quit() hook
                        error!("{e}");
                    }
                }
            }
        };
    }
    quit();
}

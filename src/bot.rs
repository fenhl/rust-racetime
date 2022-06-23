use {
    std::{
        collections::HashSet,
        convert::Infallible as Never,
        mem,
        sync::Arc,
        time::Duration,
    },
    futures::{
        SinkExt as _,
        future::{
            self,
            Future,
            TryFutureExt as _,
        },
        stream::StreamExt as _,
    },
    tokio::{
        net::TcpStream,
        sync::{
            Mutex,
            RwLock,
        },
        time::{
            MissedTickBehavior,
            interval,
            sleep,
        },
    },
    tokio_tungstenite::tungstenite::{
        self,
        client::IntoClientRequest as _,
    },
    crate::{
        Error,
        RACETIME_HOST,
        authorize,
        handler::{
            RaceContext,
            RaceHandler,
            WsStream,
        },
        http_uri,
        model::*,
    },
};

const SCAN_RACES_EVERY: Duration = Duration::from_secs(30);

struct BotData {
    category_slug: String,
    handled_races: HashSet<String>,
    client_id: String,
    client_secret: String,
    access_token: String,
    reauthorize_every: Duration,
}

#[derive(Clone)]
pub struct Bot {
    client: reqwest::Client,
    data: Arc<Mutex<BotData>>,
}

impl Bot {
    pub async fn new(category_slug: &str, client_id: &str, client_secret: &str) -> Result<Bot, Error> {
        let client = reqwest::Client::builder().user_agent(concat!("racetime-rs/", env!("CARGO_PKG_VERSION"))).build()?;
        let (access_token, reauthorize_every) = authorize(client_id, client_secret, &client).await?;
        Ok(Bot {
            client,
            data: Arc::new(Mutex::new(BotData {
                access_token, reauthorize_every,
                handled_races: HashSet::default(),
                category_slug: category_slug.to_owned(),
                client_id: client_id.to_owned(),
                client_secret: client_secret.to_owned(),
            })),
        })
    }

    /// Low-level handler for the race room. Loops over the websocket,
    /// calling the appropriate method for each message that comes in.
    async fn handle<H: RaceHandler>(mut stream: WsStream, ctx: RaceContext) -> Result<(), Error> {
        let mut handler = H::new(&ctx).await?;
        while let Some(msg_res) = stream.next().await {
            match msg_res {
                Ok(tungstenite::Message::Text(buf)) => {
                    let data = serde_json::from_str(&buf)?;
                    match data {
                        Message::ChatHistory { messages } => handler.chat_history(&ctx, messages).await?,
                        Message::ChatMessage { message } => handler.chat_message(&ctx, message).await?,
                        Message::ChatDelete { delete } => handler.chat_delete(&ctx, delete).await?,
                        Message::ChatPurge { purge } => handler.chat_purge(&ctx, purge).await?,
                        Message::Error { errors } => handler.error(&ctx, errors).await?,
                        Message::Pong => handler.pong(&ctx).await?,
                        Message::RaceData { race } => {
                            let old_race_data = mem::replace(&mut *ctx.data.write().await, race);
                            handler.race_data(&ctx, old_race_data).await?;
                        }
                        Message::RaceRenders => handler.race_renders(&ctx).await?,
                    }
                    if handler.should_stop(&ctx).await? {
                        return handler.end(&ctx).await
                    }
                }
                Ok(tungstenite::Message::Ping(payload)) => ctx.sender.lock().await.send(tungstenite::Message::Pong(payload)).await?,
                Ok(msg) => return Err(Error::UnexpectedMessageType(msg)),
                Err(e) => return Err(e.into()),
            }
        }
        Err(Error::EndOfStream)
    }

    /// Run the bot. Requires an active [`tokio`] runtime.
    pub async fn run<H: RaceHandler>(&self) -> Result<Never, Error> {
        self.run_until::<H, _, _>(future::pending()).await
    }

    /// Run the bot until the `shutdown` future resolves. Requires an active [`tokio`] runtime. `shutdown` must be cancel safe.
    pub async fn run_until<H: RaceHandler, T, Fut: Future<Output = T>>(&self, shutdown: Fut) -> Result<T, Error> {
        tokio::pin!(shutdown);
        // Divide the reauthorization interval by 2 to avoid token expiration
        let mut reauthorize = interval(self.data.lock().await.reauthorize_every / 2);
        let mut refresh_races = interval(SCAN_RACES_EVERY);
        refresh_races.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                output = &mut shutdown => return Ok(output), //TODO shut down running handlers
                _ = reauthorize.tick() => {
                    let mut data = self.data.lock().await;
                    let (access_token, reauthorize_every) = authorize(&data.client_id, &data.client_secret, &self.client).await?;
                    data.access_token = access_token;
                    data.reauthorize_every = reauthorize_every;
                    reauthorize = interval(reauthorize_every / 2);
                }
                _ = refresh_races.tick() => {
                    let data = match async { http_uri(&format!("/{}/data", self.data.lock().await.category_slug)) }
                        .and_then(|url| async { Ok(self.client.get(url).send().await?.error_for_status()?.json::<CategoryData>().await?) })
                        .await
                    {
                        Ok(data) => data,
                        Err(e) => {
                            eprintln!("Fatal error when attempting to retrieve category data: {:?}", e);
                            sleep(SCAN_RACES_EVERY).await;
                            continue
                        }
                    };
                    for summary_data in data.current_races {
                        let name = &summary_data.name;
                        let mut data = self.data.lock().await;
                        if !data.handled_races.contains(name) {
                            let race_data = match async { http_uri(&summary_data.data_url) }
                                .and_then(|url| async { Ok(self.client.get(url).send().await?.error_for_status()?.json().await?) })
                                .await
                            {
                                Ok(race_data) => race_data,
                                Err(e) => {
                                    eprintln!("Fatal error when attempting to retrieve data for race {}: {:?}", name, e);
                                    sleep(SCAN_RACES_EVERY).await;
                                    continue
                                }
                            };
                            if H::should_handle(&race_data)? {
                                let mut request = format!("wss://{RACETIME_HOST}{}", race_data.websocket_bot_url).into_client_request()?;
                                request.headers_mut().append(http::header::HeaderName::from_static("authorization"), format!("Bearer {}", data.access_token).parse()?);
                                let (ws_conn, _) = tokio_tungstenite::client_async_tls(request, TcpStream::connect((RACETIME_HOST, 443)).await?).await?;
                                data.handled_races.insert(name.to_owned());
                                drop(data);
                                let (sink, stream) = ws_conn.split();
                                let ctx = RaceContext {
                                    data: Arc::new(RwLock::new(race_data)),
                                    sender: Arc::new(Mutex::new(sink)),
                                };
                                let name = name.to_owned();
                                let data_clone = Arc::clone(&self.data);
                                tokio::spawn(async move {
                                    Self::handle::<H>(stream, ctx).await.expect("error in race handler");
                                    data_clone.lock().await.handled_races.remove(&name);
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

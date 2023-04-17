use {
    std::{
        collections::HashSet,
        convert::Infallible as Never,
        fmt,
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
        io,
        net::TcpStream,
        sync::{
            Mutex,
            RwLock,
            mpsc,
        },
        time::{
            Instant,
            MissedTickBehavior,
            interval,
            interval_at,
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
        authorize_with_host,
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

enum ErrorContext {
    ChatDelete,
    ChatHistory,
    ChatMessage,
    ChatPurge,
    Decode,
    End,
    New,
    Ping,
    Pong,
    RaceData,
    RaceRenders,
    RaceSplit,
    Reconnect,
    Recv,
    ServerError,
    ShouldStop,
}

impl fmt::Display for ErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ChatDelete => write!(f, "from chat_delete callback"),
            Self::ChatHistory => write!(f, "from chat_history callback"),
            Self::ChatMessage => write!(f, "from chat_message callback"),
            Self::ChatPurge => write!(f, "from chat_purge callback"),
            Self::Decode => write!(f, "while decoding server message"),
            Self::End => write!(f, "from end callback"),
            Self::New => write!(f, "from RaceHandler constructor"),
            Self::Ping => write!(f, "while sending ping"),
            Self::Pong => write!(f, "from pong callback"),
            Self::RaceData => write!(f, "from race_data callback"),
            Self::RaceRenders => write!(f, "from race_renders callback"),
            Self::RaceSplit => write!(f, "from race_split callback"),
            Self::Reconnect => write!(f, "while trying to reconnect"),
            Self::Recv => write!(f, "while waiting for message from server"),
            Self::ServerError => write!(f, "from error callback"),
            Self::ShouldStop => write!(f, "from should_stop callback"),
        }
    }
}

struct BotData {
    host: String,
    category_slug: String,
    handled_races: HashSet<String>,
    client_id: String,
    client_secret: String,
    access_token: String,
    reauthorize_every: Duration,
}

pub struct Bot<S: Send + Sync + ?Sized + 'static> {
    client: reqwest::Client,
    data: Arc<Mutex<BotData>>,
    state: Arc<S>,
    extra_room_tx: mpsc::Sender<String>,
    extra_room_rx: mpsc::Receiver<String>,
}

impl<S: Send + Sync + ?Sized + 'static> Bot<S> {
    pub async fn new(category_slug: &str, client_id: &str, client_secret: &str, state: Arc<S>) -> Result<Self, Error> {
        Self::new_with_host(RACETIME_HOST, category_slug, client_id, client_secret, state).await
    }

    pub async fn new_with_host(host: &str, category_slug: &str, client_id: &str, client_secret: &str, state: Arc<S>) -> Result<Self, Error> {
        let client = reqwest::Client::builder().user_agent(concat!("racetime-rs/", env!("CARGO_PKG_VERSION"))).build()?;
        let (access_token, reauthorize_every) = authorize_with_host(host, client_id, client_secret, &client).await?;
        let (extra_room_tx, extra_room_rx) = mpsc::channel(1_024);
        Ok(Bot {
            data: Arc::new(Mutex::new(BotData {
                access_token, reauthorize_every,
                handled_races: HashSet::default(),
                host: host.to_owned(),
                category_slug: category_slug.to_owned(),
                client_id: client_id.to_owned(),
                client_secret: client_secret.to_owned(),
            })),
            client, state, extra_room_tx, extra_room_rx,
        })
    }

    /// Returns a sender that takes extra room slugs (e.g. as returned from [`crate::StartRace::start`]) and has the bot handle those rooms.
    ///
    /// This can be used to have the bot handle unlisted rooms, which aren't detected automatically since they're not listed on the category detail API endpoint.
    pub fn extra_room_sender(&self) -> mpsc::Sender<String> {
        self.extra_room_tx.clone()
    }

    /// Low-level handler for the race room. Loops over the websocket,
    /// calling the appropriate method for each message that comes in.
    async fn handle<H: RaceHandler<S>>(mut stream: WsStream, ctx: RaceContext<S>, data: &Mutex<BotData>) -> Result<(), (Error, ErrorContext)> {
        async fn reconnect<S: Send + Sync + ?Sized>(last_network_error: &mut Instant, reconnect_wait_time: &mut Duration, stream: &mut WsStream, ctx: &RaceContext<S>, data: &Mutex<BotData>, reason: &str) -> Result<(), Error> {
            if last_network_error.elapsed() >= Duration::from_secs(60 * 60 * 24) {
                *reconnect_wait_time = Duration::from_secs(1); // reset wait time after no crash for a day
            } else {
                *reconnect_wait_time *= 2; // exponential backoff
            }
            eprintln!("{reason}, reconnecting in {reconnect_wait_time:?}…");
            sleep(*reconnect_wait_time).await;
            *last_network_error = Instant::now();
            let data = data.lock().await;
            let mut request = format!("wss://{}{}", data.host, ctx.data().await.websocket_bot_url).into_client_request()?;
            request.headers_mut().append(
                http::header::HeaderName::from_static("authorization"),
                format!("Bearer {}", data.access_token).parse::<http::header::HeaderValue>()?,
            );
            let (ws_conn, _) = tokio_tungstenite::client_async_tls(request, TcpStream::connect((&*data.host, 443)).await?).await?;
            drop(data);
            (*ctx.sender.lock().await, *stream) = ws_conn.split();
            Ok(())
        }

        let mut last_network_error = Instant::now();
        let mut reconnect_wait_time = Duration::from_secs(1);
        let mut handler = H::new(&ctx).await.map_err(|e| (e, ErrorContext::New))?;
        while let Some(msg_res) = stream.next().await {
            match msg_res {
                Ok(tungstenite::Message::Text(buf)) => {
                    match serde_json::from_str(&buf).map_err(|e| (e.into(), ErrorContext::Decode))? {
                        Message::ChatHistory { messages } => handler.chat_history(&ctx, messages).await.map_err(|e| (e, ErrorContext::ChatHistory))?,
                        Message::ChatMessage { message } => handler.chat_message(&ctx, message).await.map_err(|e| (e, ErrorContext::ChatMessage))?,
                        Message::ChatDelete { delete } => handler.chat_delete(&ctx, delete).await.map_err(|e| (e, ErrorContext::ChatDelete))?,
                        Message::ChatPurge { purge } => handler.chat_purge(&ctx, purge).await.map_err(|e| (e, ErrorContext::ChatPurge))?,
                        Message::Error { errors } => {
                            /*if errors.iter().all(|error| error == "Possible sync error. Refresh to continue.") {
                                // This error is not documented by racetime.gg but the wording suggests a server-side network issue.
                                // Reestablish the WebSocket connection as a workaround.
                                let data = data.lock().await;
                                let mut request = format!("wss://{}{}", data.host, ctx.data().await.websocket_bot_url).into_client_request().map_err(|e| (e.into(), ErrorContext::Reconnect))?;
                                request.headers_mut().append(
                                    http::header::HeaderName::from_static("authorization"),
                                    format!("Bearer {}", data.access_token).parse::<http::header::HeaderValue>().map_err(|e| (e.into(), ErrorContext::Reconnect))?,
                                );
                                let (ws_conn, _) = tokio_tungstenite::client_async_tls(request, TcpStream::connect((&*data.host, 443)).await.map_err(|e| (e.into(), ErrorContext::Reconnect))?).await.map_err(|e| (e.into(), ErrorContext::Reconnect))?;
                                drop(data);
                                (*ctx.sender.lock().await, stream) = ws_conn.split();
                            } else*/ // Despite what it sounds like, this is a client-side error. See also https://github.com/racetimeGG/racetime-app/pull/196
                            {
                                handler.error(&ctx, errors).await.map_err(|e| (e, ErrorContext::ServerError))?;
                            }
                        }
                        Message::Pong => handler.pong(&ctx).await.map_err(|e| (e, ErrorContext::Pong))?,
                        Message::RaceData { race } => {
                            let old_race_data = mem::replace(&mut *ctx.data.write().await, race);
                            handler.race_data(&ctx, old_race_data).await.map_err(|e| (e, ErrorContext::RaceData))?;
                        }
                        Message::RaceRenders => handler.race_renders(&ctx).await.map_err(|e| (e, ErrorContext::RaceRenders))?,
                        Message::RaceSplit => handler.race_split(&ctx).await.map_err(|e| (e, ErrorContext::RaceSplit))?,
                    }
                    if handler.should_stop(&ctx).await.map_err(|e| (e, ErrorContext::ShouldStop))? {
                        return handler.end(&ctx).await.map_err(|e| (e, ErrorContext::End))
                    }
                }
                Ok(tungstenite::Message::Ping(payload)) => ctx.sender.lock().await.send(tungstenite::Message::Pong(payload)).await.map_err(|e| (e.into(), ErrorContext::Ping))?,
                Ok(tungstenite::Message::Close(Some(tungstenite::protocol::CloseFrame { reason, .. }))) if reason == "CloudFlare WebSocket proxy restarting" => reconnect(
                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, &ctx, data,
                    "WebSocket connection closed by server",
                ).await.map_err(|e| (e, ErrorContext::Reconnect))?,
                Ok(msg) => return Err((Error::UnexpectedMessageType(msg), ErrorContext::Recv)),
                Err(tungstenite::Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => reconnect( //TODO other error kinds?
                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, &ctx, data,
                    "unexpected end of file while waiting for message form server",
                ).await.map_err(|e| (e, ErrorContext::Reconnect))?,
                Err(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)) => reconnect(
                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, &ctx, data,
                    "connection reset without closing handshake while waiting for message form server",
                ).await.map_err(|e| (e, ErrorContext::Reconnect))?,
                Err(e) => return Err((e.into(), ErrorContext::Recv)),
            }
        }
        Err((Error::EndOfStream, ErrorContext::Recv))
    }

    async fn maybe_handle_race<H: RaceHandler<S>>(&self, name: &str, data_url: &str) -> Result<(), Error> {
        let mut data = self.data.lock().await;
        if !data.handled_races.contains(name) {
            let race_data = match async { http_uri(&data.host, data_url) }
                .and_then(|url| async { Ok(self.client.get(url).send().await?.error_for_status()?.json().await?) })
                .await
            {
                Ok(race_data) => race_data,
                Err(e) => {
                    eprintln!("Fatal error when attempting to retrieve data for race {name} (retrying in {} seconds): {e:?}", SCAN_RACES_EVERY.as_secs_f64());
                    return Ok(())
                }
            };
            if H::should_handle(&race_data, Arc::clone(&self.state)).await? {
                let mut request = format!("wss://{}{}", data.host, race_data.websocket_bot_url).into_client_request()?;
                request.headers_mut().append(http::header::HeaderName::from_static("authorization"), format!("Bearer {}", data.access_token).parse()?);
                let (ws_conn, _) = tokio_tungstenite::client_async_tls(request, TcpStream::connect((&*data.host, 443)).await?).await?;
                data.handled_races.insert(name.to_owned());
                drop(data);
                let (sink, stream) = ws_conn.split();
                let race_data = Arc::new(RwLock::new(race_data));
                let ctx = RaceContext {
                    global_state: Arc::clone(&self.state),
                    data: Arc::clone(&race_data),
                    sender: Arc::new(Mutex::new(sink)),
                };
                let name = name.to_owned();
                let data_clone = Arc::clone(&self.data);
                H::task(Arc::clone(&self.state), race_data, tokio::spawn(async move {
                    if let Err((e, ctx)) = Self::handle::<H>(stream, ctx, &data_clone).await {
                        panic!("error in race handler {ctx}: {e} ({e:?})")
                    }
                    data_clone.lock().await.handled_races.remove(&name);
                })).await?;
            }
        }
        Ok(())
    }

    /// Run the bot. Requires an active [`tokio`] runtime.
    pub async fn run<H: RaceHandler<S>>(self) -> Result<Never, Error> {
        self.run_until::<H, _, _>(future::pending()).await
    }

    /// Run the bot until the `shutdown` future resolves. Requires an active [`tokio`] runtime. `shutdown` must be cancel safe.
    pub async fn run_until<H: RaceHandler<S>, T, Fut: Future<Output = T>>(mut self, shutdown: Fut) -> Result<T, Error> {
        tokio::pin!(shutdown);
        // Divide the reauthorization interval by 2 to avoid token expiration
        let reauthorize_every = self.data.lock().await.reauthorize_every / 2;
        let mut reauthorize = interval_at(Instant::now() + reauthorize_every, reauthorize_every);
        let mut refresh_races = interval(SCAN_RACES_EVERY);
        refresh_races.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                output = &mut shutdown => return Ok(output), //TODO shut down running handlers
                _ = reauthorize.tick() => {
                    let mut data = self.data.lock().await;
                    match authorize_with_host(&data.host, &data.client_id, &data.client_secret, &self.client).await {
                        Ok((access_token, reauthorize_every)) => {
                            data.access_token = access_token;
                            data.reauthorize_every = reauthorize_every;
                            reauthorize = interval_at(Instant::now() + reauthorize_every / 2, reauthorize_every / 2);
                        }
                        Err(Error::Reqwest(e)) if e.status().map_or(true, |status| status.is_server_error()) => {
                            // racetime.gg's auth endpoint has been known to return server errors intermittently, and we should also resist intermittent network errors.
                            // In those cases, we retry again after half the remaining lifetime of the current token, until that would exceed the rate limit.
                            let reauthorize_every = reauthorize.period() / 2;
                            if reauthorize_every < SCAN_RACES_EVERY { return Err(Error::Reqwest(e)) }
                            reauthorize = interval_at(Instant::now() + reauthorize_every, reauthorize_every);
                        }
                        Err(e) => return Err(e),
                    }
                }
                _ = refresh_races.tick() => {
                    let url = async {
                        let data = self.data.lock().await;
                        http_uri(&data.host, &format!("/{}/data", &data.category_slug))
                    };
                    let data = match url
                        .and_then(|url| async { Ok(self.client.get(url).send().await?.error_for_status()?.json::<CategoryData>().await?) })
                        .await
                    {
                        Ok(data) => data,
                        Err(e) => {
                            eprintln!("Error when attempting to retrieve category data (retrying in {} seconds): {e:?}", SCAN_RACES_EVERY.as_secs_f64());
                            continue
                        }
                    };
                    for summary_data in data.current_races {
                        self.maybe_handle_race::<H>(&summary_data.name, &summary_data.data_url).await?;
                    }
                }
                Some(slug) = self.extra_room_rx.recv() => {
                    let data = self.data.lock().await;
                    self.maybe_handle_race::<H>(&format!("{}/{}", data.category_slug, slug), &format!("/{}/{}/data", data.category_slug, slug)).await?;
                }
            }
        }
    }
}

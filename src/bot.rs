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
            timeout,
        },
    },
    tokio_tungstenite::tungstenite::{
        self,
        client::IntoClientRequest as _,
    },
    crate::{
        BotBuilder,
        Error,
        HostInfo,
        ReqwestResponseExt as _,
        UDuration,
        authorize_with_host,
        handler::{
            RaceContext,
            RaceHandler,
            WsStream,
        },
        model::*,
    },
};

enum ErrorContext {
    ChatDelete,
    ChatHistory,
    ChatMessage,
    ChatDm,
    ChatPin,
    ChatUnpin,
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
            Self::ChatDm => write!(f, "from chat_dm callback"),
            Self::ChatPin => write!(f, "from chat_pin callback"),
            Self::ChatUnpin => write!(f, "from chat_unpin callback"),
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
    host_info: HostInfo,
    category_slug: String,
    handled_races: HashSet<String>,
    client_id: String,
    client_secret: String,
    access_token: String,
    reauthorize_every: UDuration,
}

pub struct Bot<S: Send + Sync + ?Sized + 'static> {
    client: reqwest::Client,
    data: Arc<Mutex<BotData>>,
    state: Arc<S>,
    extra_room_tx: mpsc::Sender<String>,
    extra_room_rx: mpsc::Receiver<String>,
    scan_races_every: UDuration,
}

impl<S: Send + Sync + ?Sized + 'static> Bot<S> {
    pub async fn new(category_slug: &str, client_id: &str, client_secret: &str, state: Arc<S>) -> Result<Self, Error> {
        BotBuilder::new(category_slug, client_id, client_secret).state(state).build().await
    }

    pub async fn new_with_host(host_info: HostInfo, category_slug: &str, client_id: &str, client_secret: &str, state: Arc<S>) -> Result<Self, Error> {
        BotBuilder::new(category_slug, client_id, client_secret).state(state).host(host_info).build().await
    }

    pub(crate) async fn new_inner(builder: BotBuilder<'_, '_, '_, S>) -> Result<Self, Error> {
        let BotBuilder { category_slug, client_id, client_secret, host_info, state, user_agent, scan_races_every } = builder;
        let client = reqwest::Client::builder().user_agent(user_agent).build()?;
        let (access_token, reauthorize_every) = authorize_with_host(&host_info, client_id, client_secret, &client).await?;
        let (extra_room_tx, extra_room_rx) = mpsc::channel(1_024);
        Ok(Self {
            data: Arc::new(Mutex::new(BotData {
                access_token, reauthorize_every,
                handled_races: HashSet::default(),
                category_slug: category_slug.to_owned(),
                client_id: client_id.to_owned(),
                client_secret: client_secret.to_owned(),
                host_info,
            })),
            client, state, extra_room_tx, extra_room_rx, scan_races_every,
        })
    }

    /// Returns a sender that takes extra room slugs (e.g. as returned from [`crate::StartRace::start`]) and has the bot handle those rooms.
    ///
    /// This can be used to send known rooms to spawn their room handlers immediately rather than waiting for the next check for new rooms.
    ///
    /// In previous versions of this library, it was also necessary to use this in order to have unlisted rooms be handled. This is no longer the case since racetime.gg added an authorized API endpoint that includes unlisted rooms.
    pub fn extra_room_sender(&self) -> mpsc::Sender<String> {
        self.extra_room_tx.clone()
    }

    /// Low-level handler for the race room. Loops over the websocket,
    /// calling the appropriate method for each message that comes in.
    async fn handle<H: RaceHandler<S>>(mut stream: WsStream, ctx: RaceContext<S>, data: &Mutex<BotData>) -> Result<(), (Error, ErrorContext)> {
        async fn reconnect<S: Send + Sync + ?Sized>(last_network_error: &mut Instant, reconnect_wait_time: &mut UDuration, stream: &mut WsStream, ctx: &RaceContext<S>, data: &Mutex<BotData>, reason: &str) -> Result<(), Error> {
            let ws_conn = loop {
                if last_network_error.elapsed() >= UDuration::from_secs(60 * 60 * 24) {
                    *reconnect_wait_time = UDuration::from_secs(1); // reset wait time after no crash for a day
                } else {
                    *reconnect_wait_time *= 2; // exponential backoff
                }
                eprintln!("{reason}, reconnecting in {reconnect_wait_time:?}…");
                sleep(*reconnect_wait_time).await;
                *last_network_error = Instant::now();
                let data = data.lock().await;
                let mut request = data.host_info.websocket_uri(&ctx.data().await.websocket_bot_url)?.into_client_request()?;
                request.headers_mut().append(
                    http::header::HeaderName::from_static("authorization"),
                    format!("Bearer {}", data.access_token).parse::<http::header::HeaderValue>()?,
                );
                match tokio_tungstenite::client_async_tls(request, TcpStream::connect(data.host_info.websocket_socketaddrs()).await?).await {
                    Ok((ws_conn, _)) => break ws_conn,
                    Err(tungstenite::Error::Http(response)) if response.status().is_server_error() => continue,
                    Err(e) => return Err(e.into()),
                }
            };
            (*ctx.sender.lock().await, *stream) = ws_conn.split();
            Ok(())
        }

        let mut last_network_error = Instant::now();
        let mut reconnect_wait_time = UDuration::from_secs(1);
        let mut handler = H::new(&ctx).await.map_err(|e| (e, ErrorContext::New))?;
        loop {
            match timeout(Duration::from_secs(60 * 60), stream.next()).await {
                Ok(Some(Ok(tungstenite::Message::Text(buf)))) => {
                    match serde_json::from_str(&buf).map_err(|e| (e.into(), ErrorContext::Decode))? {
                        Message::ChatHistory { messages } => handler.chat_history(&ctx, messages).await.map_err(|e| (e, ErrorContext::ChatHistory))?,
                        Message::ChatMessage { message } => handler.chat_message(&ctx, message).await.map_err(|e| (e, ErrorContext::ChatMessage))?,
                        Message::ChatDm { message, from_user, from_bot, to } => handler.chat_dm(&ctx, message, from_user, from_bot, to).await.map_err(|e| (e, ErrorContext::ChatDm))?,
                        Message::ChatPin { message } => handler.chat_pin(&ctx, message).await.map_err(|e| (e, ErrorContext::ChatPin))?,
                        Message::ChatUnpin { message } => handler.chat_unpin(&ctx, message).await.map_err(|e| (e, ErrorContext::ChatUnpin))?,
                        Message::ChatDelete { delete } => handler.chat_delete(&ctx, delete).await.map_err(|e| (e, ErrorContext::ChatDelete))?,
                        Message::ChatPurge { purge } => handler.chat_purge(&ctx, purge).await.map_err(|e| (e, ErrorContext::ChatPurge))?,
                        Message::Error { errors } => handler.error(&ctx, errors).await.map_err(|e| (e, ErrorContext::ServerError))?,
                        Message::Pong => handler.pong(&ctx).await.map_err(|e| (e, ErrorContext::Pong))?,
                        Message::RaceData { race } => {
                            let old_race_data = mem::replace(&mut *ctx.data.write().await, race);
                            if handler.should_stop(&ctx).await.map_err(|e| (e, ErrorContext::ShouldStop))? {
                                return handler.end(&ctx).await.map_err(|e| (e, ErrorContext::End))
                            }
                            handler.race_data(&ctx, old_race_data).await.map_err(|e| (e, ErrorContext::RaceData))?;
                        }
                        Message::RaceRenders => handler.race_renders(&ctx).await.map_err(|e| (e, ErrorContext::RaceRenders))?,
                        Message::RaceSplit => handler.race_split(&ctx).await.map_err(|e| (e, ErrorContext::RaceSplit))?,
                    }
                    if handler.should_stop(&ctx).await.map_err(|e| (e, ErrorContext::ShouldStop))? {
                        return handler.end(&ctx).await.map_err(|e| (e, ErrorContext::End))
                    }
                }
                Ok(Some(Ok(tungstenite::Message::Ping(payload)))) => {
                    ctx.sender.lock().await.send(tungstenite::Message::Pong(payload)).await.map_err(|e| (e.into(), ErrorContext::Ping))?;
                    // chat stops working 1 hour after race ends, allow the handler to stop then by periodically rechecking should_stop
                    if handler.should_stop(&ctx).await.map_err(|e| (e, ErrorContext::ShouldStop))? {
                        return handler.end(&ctx).await.map_err(|e| (e, ErrorContext::End))
                    }
                }
                Ok(Some(Ok(tungstenite::Message::Close(Some(tungstenite::protocol::CloseFrame { reason, .. }))))) if matches!(&*reason, "CloudFlare WebSocket proxy restarting" | "keepalive ping timeout") => reconnect(
                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, &ctx, data,
                    "WebSocket connection closed by server",
                ).await.map_err(|e| (e, ErrorContext::Reconnect))?,
                Ok(Some(Ok(msg))) => return Err((Error::UnexpectedMessageType(msg), ErrorContext::Recv)),
                Ok(Some(Err(tungstenite::Error::Io(e)))) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof) => reconnect( //TODO other error kinds?
                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, &ctx, data,
                    "unexpected end of file while waiting for message from server",
                ).await.map_err(|e| (e, ErrorContext::Reconnect))?,
                Ok(Some(Err(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)))) => reconnect(
                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, &ctx, data,
                    "connection reset without closing handshake while waiting for message from server",
                ).await.map_err(|e| (e, ErrorContext::Reconnect))?,
                Ok(Some(Err(e))) => return Err((e.into(), ErrorContext::Recv)),
                Ok(None) => return Err((Error::EndOfStream, ErrorContext::Recv)),
                Err(tokio::time::error::Elapsed { .. }) => {
                    // chat stops working 1 hour after race ends, allow the handler to stop then by periodically rechecking should_stop
                    if handler.should_stop(&ctx).await.map_err(|e| (e, ErrorContext::ShouldStop))? {
                        return handler.end(&ctx).await.map_err(|e| (e, ErrorContext::End))
                    }
                }
            }
        }
    }

    async fn maybe_handle_race<H: RaceHandler<S>>(&self, name: &str, data_url: &str) -> Result<(), Error> {
        let mut data = self.data.lock().await;
        if !data.handled_races.contains(name) {
            let race_data = match async { data.host_info.http_uri(data_url) }
                .and_then(|url| async { Ok(self.client.get(url).send().await?.detailed_error_for_status().await?.json().await?) })
                .await
            {
                Ok(race_data) => race_data,
                Err(e) => {
                    eprintln!("Fatal error when attempting to retrieve data for race {name} (retrying in {} seconds): {e:?}", self.scan_races_every.as_secs_f64());
                    return Ok(())
                }
            };
            if H::should_handle(&race_data, Arc::clone(&self.state)).await? {
                let mut request = data.host_info.websocket_uri(&race_data.websocket_bot_url)?.into_client_request()?;
                request.headers_mut().append(http::header::HeaderName::from_static("authorization"), format!("Bearer {}", data.access_token).parse()?);
                let (ws_conn, _) = tokio_tungstenite::client_async_tls(
                    request, TcpStream::connect(data.host_info.websocket_socketaddrs()).await?,
                ).await?;
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
        let mut refresh_races = interval(self.scan_races_every);
        refresh_races.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                output = &mut shutdown => return Ok(output), //TODO shut down running handlers
                _ = reauthorize.tick() => {
                    let mut data = self.data.lock().await;
                    match authorize_with_host(&data.host_info, &data.client_id, &data.client_secret, &self.client).await {
                        Ok((access_token, reauthorize_every)) => {
                            data.access_token = access_token;
                            data.reauthorize_every = reauthorize_every;
                            reauthorize = interval_at(Instant::now() + reauthorize_every / 2, reauthorize_every / 2);
                        }
                        Err(Error::Reqwest(e)) if e.status().map_or(true, |status| status.is_server_error()) => {
                            // racetime.gg's auth endpoint has been known to return server errors intermittently, and we should also resist intermittent network errors.
                            // In those cases, we retry again after half the remaining lifetime of the current token, until that would exceed the rate limit.
                            let reauthorize_every = reauthorize.period() / 2;
                            if reauthorize_every < self.scan_races_every { return Err(Error::Reqwest(e)) }
                            reauthorize = interval_at(Instant::now() + reauthorize_every, reauthorize_every);
                        }
                        Err(e) => return Err(e),
                    }
                }
                _ = refresh_races.tick() => {
                    let request_builder = async {
                        let data = self.data.lock().await;
                        data.host_info.http_uri(&format!("/o/{}/data", &data.category_slug)).map(|url| self.client.get(url).bearer_auth(&data.access_token))
                    };
                    let data = match request_builder
                        .and_then(|request_builder| async { Ok(request_builder.send().await?.detailed_error_for_status().await?.json::<CategoryData>().await?) })
                        .await
                    {
                        Ok(data) => data,
                        Err(e) => {
                            eprintln!("Error when attempting to retrieve category data (retrying in {} seconds): {e:?}", self.scan_races_every.as_secs_f64());
                            continue
                        }
                    };
                    for summary_data in data.current_races {
                        self.maybe_handle_race::<H>(&summary_data.name, &summary_data.data_url).await?;
                    }
                }
                Some(slug) = self.extra_room_rx.recv() => {
                    let (name, data_url) = {
                        let data = self.data.lock().await;
                        (format!("{}/{}", data.category_slug, slug), format!("/{}/{}/data", data.category_slug, slug))
                    };
                    self.maybe_handle_race::<H>(&name, &data_url).await?;
                }
            }
        }
    }
}

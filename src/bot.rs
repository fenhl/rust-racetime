use {
    std::{
        collections::HashSet,
        convert::Infallible as Never,
        fmt,
        mem,
        sync::Arc,
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
            error::Elapsed,
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
        AuthError,
        BotBuilder,
        HostInfo,
        UDuration,
        authorize_with_host,
        handler::{
            RaceContext,
            RaceHandler,
            WsStream,
        },
        maybe_timeout,
        model::*,
    },
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleErrorContext {
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

impl fmt::Display for HandleErrorContext {
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

#[derive(Debug, thiserror::Error)]
pub enum HandleErrorKind<H> {
    #[error(transparent)] Elapsed(#[from] Elapsed),
    #[error(transparent)] Handler(H),
    #[error(transparent)] InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Url(#[from] url::ParseError),
    #[error(transparent)] WebSocket(#[from] tungstenite::Error),
    #[error("failed to connect to WebSocket endpoint: {0}")]
    Connect(#[source] io::Error),
    #[error("websocket connection closed by the server")]
    EndOfStream,
    #[error("expected text message from websocket, but received {0:?}")]
    UnexpectedMessageType(tungstenite::Message),
}

#[derive(Debug, thiserror::Error)]
#[error("{ctx}: {source}")]
pub struct HandleError<H> {
    pub ctx: HandleErrorContext,
    pub source: HandleErrorKind<H>,
}

trait HandleResultExt<H> {
    type Ok;

    fn at(self, ctx: HandleErrorContext) -> Result<Self::Ok, HandleError<H>>;
}

impl<H, T, E: Into<HandleErrorKind<H>>> HandleResultExt<H> for Result<T, E> {
    type Ok = T;

    fn at(self, ctx: HandleErrorContext) -> Result<T, HandleError<H>> {
        self.map_err(|e| HandleError { ctx, source: e.into() })
    }
}

trait HandlerResultExt {
    type Ok;
    type Err;

    fn h_at(self, ctx: HandleErrorContext) -> Result<Self::Ok, HandleError<Self::Err>>;
}

impl<T, E> HandlerResultExt for Result<T, E> {
    type Ok = T;
    type Err = E;

    fn h_at(self, ctx: HandleErrorContext) -> Result<T, HandleError<E>> {
        self.map_err(|e| HandleError { ctx, source: HandleErrorKind::Handler(e) })
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
    network_timeout: Option<UDuration>,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError<H> {
    #[error(transparent)] Auth(#[from] AuthError),
    #[error(transparent)] Elapsed(#[from] Elapsed),
    #[error(transparent)] Handler(H),
    #[error(transparent)] Http(#[from] reqwest::Error),
    #[error(transparent)] InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error(transparent)] Url(#[from] url::ParseError),
    #[error(transparent)] WebSocket(#[from] tungstenite::Error),
    #[error("failed to connect to WebSocket endpoint: {0}")]
    Connect(#[source] io::Error),
    #[error("{inner}, body:\n\n{}", .text.as_ref().map(|text| text.clone()).unwrap_or_else(|e| e.to_string()))]
    ResponseStatus {
        #[source]
        inner: reqwest::Error,
        headers: reqwest::header::HeaderMap,
        text: reqwest::Result<String>,
    },
}

impl<S: Send + Sync + ?Sized + 'static> Bot<S> {
    pub async fn new(category_slug: &str, client_id: &str, client_secret: &str, state: Arc<S>) -> Result<Self, AuthError> {
        BotBuilder::new(category_slug, client_id, client_secret).state(state).build().await
    }

    pub async fn new_with_host(host_info: HostInfo, category_slug: &str, client_id: &str, client_secret: &str, state: Arc<S>) -> Result<Self, AuthError> {
        BotBuilder::new(category_slug, client_id, client_secret).state(state).host(host_info).build().await
    }

    pub(crate) async fn new_inner(builder: BotBuilder<'_, '_, '_, S>) -> Result<Self, AuthError> {
        let BotBuilder { category_slug, client_id, client_secret, host_info, state, user_agent, scan_races_every, network_timeout } = builder;
        let mut client = reqwest::Client::builder().user_agent(user_agent);
        if let Some(network_timeout) = network_timeout {
            client = client.timeout(network_timeout);
        }
        let client = client.build()?;
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
            client, state, extra_room_tx, extra_room_rx, scan_races_every, network_timeout,
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
    async fn handle<H: RaceHandler<S>>(mut stream: WsStream, network_timeout: Option<UDuration>, ctx: RaceContext<S>, data: &Mutex<BotData>) -> Result<(), HandleError<H::Error>> {
        async fn reconnect<S: Send + Sync + ?Sized, E>(last_network_error: &mut Instant, reconnect_wait_time: &mut UDuration, stream: &mut WsStream, network_timeout: Option<UDuration>, ctx: &RaceContext<S>, data: &Mutex<BotData>, reason: &str) -> Result<(), HandleErrorKind<E>> {
            enum ReconnectAttemptError {
                Connect(io::Error),
                WebSocket(tungstenite::Error),
            }

            let ws_conn = loop {
                if last_network_error.elapsed() >= UDuration::from_hours(24) {
                    *reconnect_wait_time = UDuration::from_secs(1); // reset wait time after no crash for a day
                } else {
                    *reconnect_wait_time = reconnect_wait_time.saturating_mul(2).min(UDuration::from_mins(5)); // exponential backoff
                }
                eprintln!("{reason}, reconnecting in {reconnect_wait_time:?}…");
                sleep(*reconnect_wait_time).await;
                *last_network_error = Instant::now();
                let (host_info, access_token) = {
                    let data = data.lock().await;
                    (data.host_info.clone(), data.access_token.clone())
                };
                let mut request = host_info.websocket_uri(&ctx.data().await.websocket_bot_url)?.into_client_request()?;
                request.headers_mut().append(
                    http::header::HeaderName::from_static("authorization"),
                    format!("Bearer {access_token}").parse::<http::header::HeaderValue>()?,
                );
                match maybe_timeout(network_timeout, async {
                    let tcp_stream = TcpStream::connect(host_info.websocket_socketaddrs()).await.map_err(ReconnectAttemptError::Connect)?;
                    tokio_tungstenite::client_async_tls(request, tcp_stream).await
                        .map(|(ws_conn, _)| ws_conn)
                        .map_err(ReconnectAttemptError::WebSocket)
                }).await {
                    Ok(Ok(ws_conn)) => break ws_conn,
                    Ok(Err(ReconnectAttemptError::Connect(e))) => eprintln!("WebSocket reconnect attempt failed to connect: {e}"),
                    Ok(Err(ReconnectAttemptError::WebSocket(e))) => eprintln!("WebSocket reconnect attempt failed: {e}"),
                    Err(Elapsed { .. }) => eprintln!("WebSocket reconnect attempt timed out"),
                }
            };
            (*ctx.sender.lock().await, *stream) = ws_conn.split();
            Ok(())
        }

        let mut last_network_error = Instant::now();
        let mut reconnect_wait_time = UDuration::from_secs(1);
        let mut waiting_for_probe_response = false;
        let mut handler = H::new(&ctx).await.h_at(HandleErrorContext::New)?;
        loop {
            match maybe_timeout(network_timeout, stream.next()).await {
                Ok(Some(Ok(tungstenite::Message::Text(buf)))) => {
                    waiting_for_probe_response = false;
                    match serde_json::from_str(&buf).at(HandleErrorContext::Decode)? {
                        Message::ChatHistory { messages } => handler.chat_history(&ctx, messages).await.h_at(HandleErrorContext::ChatHistory)?,
                        Message::ChatMessage { message } => handler.chat_message(&ctx, message).await.h_at(HandleErrorContext::ChatMessage)?,
                        Message::ChatDm { message, from_user, from_bot, to } => handler.chat_dm(&ctx, message, from_user, from_bot, to).await.h_at(HandleErrorContext::ChatDm)?,
                        Message::ChatPin { message } => handler.chat_pin(&ctx, message).await.h_at(HandleErrorContext::ChatPin)?,
                        Message::ChatUnpin { message } => handler.chat_unpin(&ctx, message).await.h_at(HandleErrorContext::ChatUnpin)?,
                        Message::ChatDelete { delete } => handler.chat_delete(&ctx, delete).await.h_at(HandleErrorContext::ChatDelete)?,
                        Message::ChatPurge { purge } => handler.chat_purge(&ctx, purge).await.h_at(HandleErrorContext::ChatPurge)?,
                        Message::Error { errors } => handler.error(&ctx, errors).await.h_at(HandleErrorContext::ServerError)?,
                        Message::Pong => handler.pong(&ctx).await.h_at(HandleErrorContext::Pong)?,
                        Message::RaceData { race } => {
                            let old_race_data = mem::replace(&mut *ctx.data.write().await, race);
                            if handler.should_stop(&ctx).await.h_at(HandleErrorContext::ShouldStop)? {
                                return handler.end(&ctx).await.h_at(HandleErrorContext::End)
                            }
                            handler.race_data(&ctx, old_race_data).await.h_at(HandleErrorContext::RaceData)?;
                        }
                        Message::RaceRenders => handler.race_renders(&ctx).await.h_at(HandleErrorContext::RaceRenders)?,
                        Message::RaceSplit => handler.race_split(&ctx).await.h_at(HandleErrorContext::RaceSplit)?,
                    }
                    if handler.should_stop(&ctx).await.h_at(HandleErrorContext::ShouldStop)? {
                        return handler.end(&ctx).await.h_at(HandleErrorContext::End)
                    }
                }
                Ok(Some(Ok(tungstenite::Message::Ping(payload)))) => {
                    waiting_for_probe_response = false;
                    maybe_timeout(network_timeout, ctx.sender.lock().await.send(tungstenite::Message::Pong(payload))).await.at(HandleErrorContext::Ping)?.at(HandleErrorContext::Ping)?;
                    // chat stops working 1 hour after race ends, allow the handler to stop then by periodically rechecking should_stop
                    if handler.should_stop(&ctx).await.h_at(HandleErrorContext::ShouldStop)? {
                        return handler.end(&ctx).await.h_at(HandleErrorContext::End)
                    }
                }
                Ok(Some(Ok(tungstenite::Message::Pong(_)))) => waiting_for_probe_response = false,
                Ok(Some(Ok(tungstenite::Message::Close(Some(tungstenite::protocol::CloseFrame { reason, .. }))))) if matches!(&*reason, "CloudFlare WebSocket proxy restarting" | "keepalive ping timeout") => {
                    reconnect(
                        &mut last_network_error, &mut reconnect_wait_time, &mut stream, network_timeout, &ctx, data,
                        "WebSocket connection closed by server",
                    ).await.at(HandleErrorContext::Reconnect)?;
                    waiting_for_probe_response = false;
                }
                Ok(Some(Ok(msg))) => return Err(HandleErrorKind::UnexpectedMessageType(msg)).at(HandleErrorContext::Recv),
                Ok(Some(Err(tungstenite::Error::Io(e)))) if matches!(e.kind(), io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof) => {
                    reconnect( //TODO other error kinds?
                        &mut last_network_error, &mut reconnect_wait_time, &mut stream, network_timeout, &ctx, data,
                        "unexpected end of file while waiting for message from server",
                    ).await.at(HandleErrorContext::Reconnect)?;
                    waiting_for_probe_response = false;
                }
                Ok(Some(Err(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)))) => {
                    reconnect(
                        &mut last_network_error, &mut reconnect_wait_time, &mut stream, network_timeout, &ctx, data,
                        "connection reset without closing handshake while waiting for message from server",
                    ).await.at(HandleErrorContext::Reconnect)?;
                    waiting_for_probe_response = false;
                }
                Ok(Some(Err(e))) => return Err(e).at(HandleErrorContext::Recv),
                Ok(None) => return Err(HandleErrorKind::EndOfStream).at(HandleErrorContext::Recv),
                Err(Elapsed { .. }) => {
                    // chat stops working 1 hour after race ends, allow the handler to stop then by periodically rechecking should_stop
                    if handler.should_stop(&ctx).await.h_at(HandleErrorContext::ShouldStop)? {
                        return handler.end(&ctx).await.h_at(HandleErrorContext::End)
                    }
                    if waiting_for_probe_response {
                        reconnect(
                            &mut last_network_error, &mut reconnect_wait_time, &mut stream, network_timeout, &ctx, data,
                            "no response to WebSocket liveness probe",
                        ).await.at(HandleErrorContext::Reconnect)?;
                        waiting_for_probe_response = false;
                    } else {
                        match maybe_timeout(network_timeout, ctx.sender.lock().await.send(tungstenite::Message::Ping(Default::default()))).await {
                            Ok(Ok(())) => waiting_for_probe_response = true,
                            Ok(Err(e)) => {
                                let reason = format!("failed to send WebSocket liveness probe: {e}");
                                reconnect(
                                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, network_timeout, &ctx, data,
                                    &reason,
                                ).await.at(HandleErrorContext::Reconnect)?;
                                waiting_for_probe_response = false;
                            }
                            Err(Elapsed { .. }) => {
                                reconnect(
                                    &mut last_network_error, &mut reconnect_wait_time, &mut stream, network_timeout, &ctx, data,
                                    "timed out sending WebSocket liveness probe",
                                ).await.at(HandleErrorContext::Reconnect)?;
                                waiting_for_probe_response = false;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn maybe_handle_race<H: RaceHandler<S>>(&self, name: &str, data_url: &str) -> Result<(), RunError<H::Error>> {
        let mut data = self.data.lock().await;
        if !data.handled_races.contains(name) {
            let race_data = match async { data.host_info.http_uri(data_url) }
                .err_into::<RunError<H::Error>>()
                .and_then(|url| async {
                    let response = self.client.get(url).send().await?;
                    match response.error_for_status_ref() {
                        Ok(_) => Ok(response.json().await?),
                        Err(inner) => Err(RunError::ResponseStatus {
                            headers: response.headers().clone(),
                            text: response.text().await,
                            inner,
                        }),
                    }
                })
                .await
            {
                Ok(race_data) => race_data,
                Err(e) => {
                    eprintln!("Fatal error when attempting to retrieve data for race {name} (retrying in {} seconds): {e:?}", self.scan_races_every.as_secs_f64());
                    return Ok(())
                }
            };
            if H::should_handle(&race_data, Arc::clone(&self.state)).await.map_err(RunError::Handler)? {
                let mut request = data.host_info.websocket_uri(&race_data.websocket_bot_url)?.into_client_request()?;
                request.headers_mut().append(http::header::HeaderName::from_static("authorization"), format!("Bearer {}", data.access_token).parse()?);
                let (ws_conn, _) = tokio_tungstenite::client_async_tls(
                    request, maybe_timeout(self.network_timeout, TcpStream::connect(data.host_info.websocket_socketaddrs())).await?.map_err(RunError::Connect)?,
                ).await?;
                data.handled_races.insert(name.to_owned());
                drop(data);
                let (sink, stream) = ws_conn.split();
                let network_timeout = self.network_timeout;
                let race_data = Arc::new(RwLock::new(race_data));
                let ctx = RaceContext {
                    global_state: Arc::clone(&self.state),
                    data: Arc::clone(&race_data),
                    sender: Arc::new(Mutex::new(sink)),
                    network_timeout,
                };
                let name = name.to_owned();
                let data_clone = Arc::clone(&self.data);
                H::task(Arc::clone(&self.state), race_data, tokio::spawn(async move {
                    let result = Self::handle::<H>(stream, network_timeout, ctx, &data_clone).await;
                    data_clone.lock().await.handled_races.remove(&name);
                    result
                })).await.map_err(RunError::Handler)?;
            }
        }
        Ok(())
    }

    /// Run the bot. Requires an active [`tokio`] runtime.
    pub async fn run<H: RaceHandler<S>>(self) -> Result<Never, RunError<H::Error>> {
        self.run_until::<H, _, _>(future::pending()).await
    }

    /// Run the bot until the `shutdown` future resolves. Requires an active [`tokio`] runtime. `shutdown` must be cancel safe.
    pub async fn run_until<H: RaceHandler<S>, T, Fut: Future<Output = T>>(mut self, shutdown: Fut) -> Result<T, RunError<H::Error>> {
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
                        Err(AuthError::Http(e)) if e.status().map_or(true, |status| status.is_server_error()) => {
                            // racetime.gg's auth endpoint has been known to return server errors intermittently, and we should also resist intermittent network errors.
                            // In those cases, we retry again after half the remaining lifetime of the current token, until that would exceed the rate limit.
                            let reauthorize_every = reauthorize.period() / 2;
                            if reauthorize_every < self.scan_races_every { return Err(AuthError::Http(e).into()) }
                            reauthorize = interval_at(Instant::now() + reauthorize_every, reauthorize_every);
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
                _ = refresh_races.tick() => {
                    let request_builder = async {
                        let data = self.data.lock().await;
                        data.host_info.http_uri(&format!("/o/{}/data", &data.category_slug)).map(|url| self.client.get(url).bearer_auth(&data.access_token))
                    };
                    let data = match request_builder
                        .err_into::<RunError<H::Error>>()
                        .and_then(|request_builder| async {
                            let response = request_builder.send().await?;
                            match response.error_for_status_ref() {
                                Ok(_) => Ok(response.json::<CategoryData>().await?),
                                Err(inner) => Err(RunError::ResponseStatus {
                                    headers: response.headers().clone(),
                                    text: response.text().await,
                                    inner,
                                }),
                            }
                        })
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

#[cfg(test)]
mod tests {
    use {
        std::{
            borrow::Cow,
            num::NonZeroU16,
            sync::{
                Arc,
                atomic::{
                    AtomicBool,
                    Ordering,
                },
            },
        },
        async_trait::async_trait,
        futures::{
            SinkExt as _,
            StreamExt as _,
        },
        tokio::{
            net::TcpListener,
            time::advance,
        },
        tokio_tungstenite::{
            accept_async,
            connect_async,
            tungstenite,
        },
        crate::handler::ServerErrors,
        super::*,
    };

    #[derive(Default)]
    struct TestState {
        ended: AtomicBool,
    }

    #[derive(Debug, thiserror::Error)]
    #[error("test handler error")]
    struct TestHandlerError;

    impl From<ServerErrors> for TestHandlerError {
        fn from(_: ServerErrors) -> Self {
            Self
        }
    }

    struct TestHandler;

    #[async_trait]
    impl RaceHandler<TestState> for TestHandler {
        type Error = TestHandlerError;

        async fn new(_: &RaceContext<TestState>) -> Result<Self, Self::Error> {
            Ok(Self)
        }

        async fn end(self, ctx: &RaceContext<TestState>) -> Result<(), Self::Error> {
            ctx.global_state.ended.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    fn race_data_json(status: &str) -> serde_json::Value {
        let mut data = serde_json::Map::new();
        for part in [
            serde_json::json!({
                "version": 1,
                "name": "test/test-room",
                "slug": "test-room",
                "category": {
                    "name": "Test",
                    "short_name": "Test",
                    "slug": "test",
                    "url": "/test",
                    "data_url": "/test/data"
                },
                "status": {
                    "value": status,
                    "verbose_value": status,
                    "help_text": ""
                },
                "url": "/test/test-room",
                "data_url": "/test/test-room/data",
                "websocket_url": "/ws",
                "websocket_bot_url": "/ws",
                "websocket_oauth_url": "/ws",
                "goal": {
                    "name": "Test",
                    "custom": false
                },
                "info": "",
                "info_bot": null,
                "info_user": null
            }),
            serde_json::json!({
                "entrants_count": 0,
                "entrants_count_finished": 0,
                "entrants_count_inactive": 0,
                "entrants": [],
                "opened_at": "2026-01-01T00:00:00Z",
                "start_delay": "P0DT0H0M0S",
                "started_at": null,
                "ended_at": if status == "finished" { Some("2026-01-01T01:00:00Z") } else { None },
                "cancelled_at": null,
                "ranked": false,
                "unlisted": false,
                "time_limit": "P0DT24H0M0S",
                "time_limit_auto_complete": false,
                "require_even_teams": false,
                "streaming_required": false
            }),
            serde_json::json!({
                "auto_start": false,
                "opened_by": null,
                "monitors": [],
                "recordable": false,
                "recorded": false,
                "recorded_by": null,
                "disqualify_unready": false,
                "allow_comments": true,
                "hide_comments": false,
                "hide_entrants": false,
                "chat_restricted": false,
                "allow_midrace_chat": true,
                "allow_non_entrant_chat": true,
                "chat_message_delay": "P0DT0H0M0S",
                "bot_meta": {}
            }),
        ] {
            let serde_json::Value::Object(part) = part else { unreachable!() };
            data.extend(part);
        }
        serde_json::Value::Object(data)
    }

    fn context(
        ws_conn: tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>,
        host_info: HostInfo,
        state: Arc<TestState>,
        network_timeout: UDuration,
    ) -> (WsStream, RaceContext<TestState>, Arc<Mutex<BotData>>) {
        let (sink, stream) = ws_conn.split();
        let race_data = serde_json::from_value(race_data_json("in_progress")).expect("invalid test race data");
        let ctx = RaceContext {
            global_state: state,
            data: Arc::new(RwLock::new(race_data)),
            sender: Arc::new(Mutex::new(sink)),
            network_timeout: Some(network_timeout),
        };
        let data = Arc::new(Mutex::new(BotData {
            host_info,
            category_slug: "test".to_owned(),
            handled_races: HashSet::from(["test/test-room".to_owned()]),
            client_id: "client".to_owned(),
            client_secret: "secret".to_owned(),
            access_token: "token".to_owned(),
            reauthorize_every: UDuration::from_hours(1),
        }));
        (stream, ctx, data)
    }

    #[tokio::test(start_paused = true)]
    async fn server_keepalives_do_not_trigger_liveness_probes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("failed to bind test listener");
        let port = listener.local_addr().expect("test listener has no address").port();
        let url = format!("ws://127.0.0.1:{port}/ws");
        let (server_result, client_result) = tokio::join!(
            async {
                let (tcp_stream, _) = listener.accept().await.expect("failed to accept initial connection");
                accept_async(tcp_stream).await
            },
            connect_async(&url),
        );
        let mut server_stream = server_result.expect("failed to accept initial WebSocket");
        let (client_stream, _) = client_result.expect("failed to connect initial WebSocket");
        let network_timeout = UDuration::from_millis(50);
        let state = Arc::new(TestState::default());
        let host_info = HostInfo {
            hostname: Cow::Borrowed("127.0.0.1"),
            port: NonZeroU16::new(port).expect("test listener chose port 0"),
            secure: false,
        };
        let (stream, ctx, data) = context(client_stream, host_info, Arc::clone(&state), network_timeout);
        let handle_task = tokio::spawn(async move {
            Bot::<TestState>::handle::<TestHandler>(stream, Some(network_timeout), ctx, &data).await
        });
        let server_task = tokio::spawn(async move {
            for payload in [b"one".as_slice(), b"two", b"three"] {
                sleep(network_timeout / 2).await;
                server_stream.send(tungstenite::Message::Ping(payload.to_vec().into())).await.expect("failed to send server keepalive");
                let response = server_stream.next().await;
                assert!(
                    matches!(&response, Some(Ok(tungstenite::Message::Pong(response))) if response == payload),
                    "client sent an unexpected frame instead of the keepalive response: {response:?}",
                );
            }
            let message = serde_json::json!({
                "type": "race.data",
                "race": race_data_json("finished")
            });
            server_stream.send(tungstenite::Message::Text(message.to_string().into())).await.expect("failed to send finished race data");
        });

        for _ in 0..100 {
            advance(network_timeout / 10).await;
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            if handle_task.is_finished() && server_task.is_finished() {
                break
            }
        }

        assert!(handle_task.is_finished() && server_task.is_finished(), "healthy WebSocket test did not finish");
        server_task.await.expect("test WebSocket server panicked");
        handle_task.await.expect("race handler panicked").expect("race handler errored");
        assert!(state.ended.load(Ordering::SeqCst), "handler did not process the finished race");
    }

    #[tokio::test(start_paused = true)]
    async fn silent_connection_reconnects_until_fresh_race_data_arrives() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("failed to bind test listener");
        let port = listener.local_addr().expect("test listener has no address").port();
        let url = format!("ws://127.0.0.1:{port}/ws");
        let (server_result, client_result) = tokio::join!(
            async {
                let (tcp_stream, _) = listener.accept().await.expect("failed to accept initial connection");
                accept_async(tcp_stream).await
            },
            connect_async(&url),
        );
        let initial_server_stream = server_result.expect("failed to accept initial WebSocket");
        let (client_stream, _) = client_result.expect("failed to connect initial WebSocket");
        let network_timeout = UDuration::from_millis(50);
        let state = Arc::new(TestState::default());
        let host_info = HostInfo {
            hostname: Cow::Borrowed("127.0.0.1"),
            port: NonZeroU16::new(port).expect("test listener chose port 0"),
            secure: false,
        };
        let (stream, ctx, data) = context(client_stream, host_info, Arc::clone(&state), network_timeout);
        let handle_task = tokio::spawn(async move {
            Bot::<TestState>::handle::<TestHandler>(stream, Some(network_timeout), ctx, &data).await
        });
        let server_task = tokio::spawn(async move {
            let _initial_server_stream = initial_server_stream; // keep the silent connection open

            let (stalled_tcp_stream, _) = listener.accept().await.expect("failed to accept stalled reconnect");
            sleep(network_timeout * 2).await; // force the full reconnect handshake to time out
            drop(stalled_tcp_stream);

            let (tcp_stream, _) = listener.accept().await.expect("failed to accept successful reconnect");
            let mut server_stream = accept_async(tcp_stream).await.expect("failed to accept reconnected WebSocket");
            let message = serde_json::json!({
                "type": "race.data",
                "race": race_data_json("finished")
            });
            server_stream.send(tungstenite::Message::Text(message.to_string().into())).await.expect("failed to send fresh race data");
        });

        for _ in 0..1_000 {
            advance(UDuration::from_millis(10)).await;
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
            if handle_task.is_finished() && server_task.is_finished() {
                break
            }
        }

        assert!(handle_task.is_finished() && server_task.is_finished(), "reconnect test did not finish");
        server_task.await.expect("test WebSocket server panicked");
        handle_task.await.expect("race handler panicked").expect("race handler errored");
        assert!(state.ended.load(Ordering::SeqCst), "handler did not process the fresh finished race data");
    }
}

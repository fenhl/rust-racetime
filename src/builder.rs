use {
    std::sync::Arc,
    crate::{
        AuthError,
        Bot,
        HostInfo,
        UDuration,
    },
};

pub struct BotBuilder<'a, 'b, 'c, S: Send + Sync + ?Sized + 'static> {
    pub(crate) category_slug: &'a str,
    pub(crate) client_id: &'b str,
    pub(crate) client_secret: &'c str,
    pub(crate) host_info: HostInfo,
    pub(crate) state: Arc<S>,
    pub(crate) user_agent: &'static str,
    pub(crate) scan_races_every: UDuration,
    pub(crate) network_timeout: Option<UDuration>,
}

impl<'a, 'b, 'c> BotBuilder<'a, 'b, 'c, ()> {
    pub fn new(category_slug: &'a str, client_id: &'b str, client_secret: &'c str) -> Self {
        Self {
            host_info: HostInfo::default(),
            state: Arc::default(),
            user_agent: concat!("racetime-rs/", env!("CARGO_PKG_VERSION")),
            scan_races_every: UDuration::from_secs(30),
            network_timeout: Some(UDuration::from_hours(1)),
            category_slug, client_id, client_secret,
        }
    }

    pub fn state<S: Send + Sync + ?Sized + 'static>(self, state: Arc<S>) -> BotBuilder<'a, 'b, 'c, S> {
        let Self { category_slug, client_id, client_secret, host_info, state: _, user_agent, scan_races_every, network_timeout } = self;
        BotBuilder { category_slug, client_id, client_secret, host_info, state, user_agent, scan_races_every, network_timeout }
    }
}

impl<S: Send + Sync + ?Sized + 'static> BotBuilder<'_, '_, '_, S> {
    pub fn host(self, host_info: HostInfo) -> Self {
        Self { host_info, ..self }
    }

    #[doc = concat!("Defaults to `racetime-rs/", env!("CARGO_PKG_VERSION"), "`.")]
    pub fn user_agent(self, user_agent: &'static str) -> Self {
        Self { user_agent, ..self }
    }

    /// According to <https://github.com/racetimeGG/racetime-app/issues/217#issuecomment-2915924787> this can be set as low as 5 seconds without hitting rate limits.
    /// Defaults to 30 seconds.
    pub fn scan_races_every(self, scan_races_every: UDuration) -> Self {
        Self { scan_races_every, ..self }
    }

    /// Consider it an error if any network operation (HTTP request or WebSocket write) takes longer than this.
    ///
    /// After a WebSocket read is silent for this long, the bot sends one liveness probe. If the connection remains silent
    /// for another interval, it reconnects. No probes are sent while the server's normal keepalive messages are arriving.
    /// Defaults to 1 hour.
    pub fn network_timeout(self, network_timeout: Option<UDuration>) -> Self {
        Self { network_timeout, ..self }
    }

    pub async fn build(self) -> Result<Bot<S>, AuthError> {
        Bot::new_inner(self).await
    }
}

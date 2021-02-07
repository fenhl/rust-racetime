use {
    std::{
        collections::{
            BTreeMap,
            HashSet,
        },
        convert::Infallible as Never,
        fmt,
        io,
        sync::Arc,
        time::Duration,
    },
    collect_mac::collect,
    derive_more::From,
    futures::{
        future::TryFutureExt as _,
        stream::StreamExt as _,
    },
    serde::Deserialize,
    tokio::{
        net::TcpStream,
        sync::Mutex,
        task::JoinError,
        time::sleep,
    },
    url::Url,
    crate::{
        handler::{
            self,
            RaceHandler,
            WsStream,
        },
        model::{
            CategoryData,
            RaceData,
        },
    },
};

const RACETIME_HOST: &str = "racetime.gg";
const SCAN_RACES_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, From)]
pub enum Error {
    Handler(handler::Error),
    Io(io::Error),
    Reqwest(reqwest::Error),
    Task(JoinError),
    Tungstenite(tokio_tungstenite::tungstenite::Error),
    UrlParse(url::ParseError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Handler(e) => write!(f, "error in race handler: {}", e),
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::Reqwest(e) => if let Some(url) = e.url() {
                write!(f, "HTTP error at {}: {}", url, e)
            } else {
                write!(f, "HTTP error: {}", e)
            },
            Error::Task(e) => e.fmt(f),
            Error::Tungstenite(e) => write!(f, "websocket error: {}", e),
            Error::UrlParse(e) => e.fmt(f),
        }
    }
}

#[derive(Deserialize)]
struct AuthResponse {
    access_token: String,
    expires_in: Option<u64>,
}

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
        let (access_token, reauthorize_every) = Self::authorize(client_id, client_secret, &client).await?;
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

    /// Get an OAuth2 token from the authentication server.
    async fn authorize(client_id: &str, client_secret: &str, client: &reqwest::Client) -> Result<(String, Duration), Error> {
        let data = client.post(http_uri("/o/token")?)
            .form(&collect![as BTreeMap<_, _>:
                "client_id" => client_id,
                "client_secret" => client_secret,
                "grant_type" => "client_credentials",
            ])
            .send().await?
            .error_for_status()?
            .json::<AuthResponse>().await?;
        Ok((
            data.access_token,
            Duration::from_secs(data.expires_in.unwrap_or(36000)),
        ))
    }

    /// Create a new WebSocket connection and set up a handler object to manage
    /// it.
    async fn create_handler<H: RaceHandler>(&self, race_data: RaceData) -> Result<(H, WsStream), Error> {
        let (ws_conn, _) = tokio_tungstenite::client_async_tls(
            httparse::Request {
                method: Some("GET"),
                path: Some(&race_data.websocket_bot_url),
                version: None,
                headers: &mut [httparse::Header { name: "Authorization", value: format!("Bearer {}", self.data.lock().await.access_token).as_bytes() }],
            },
            TcpStream::connect(RACETIME_HOST).await?,
        ).await?;
        let (sink, stream) = ws_conn.split();
        Ok((H::new(race_data, sink)?, stream))
    }

    /// Reauthorize with the token endpoint, to generate a new access token
    /// before the current one expires.
    ///
    /// This method runs in a constant loop, creating a new access token when
    /// needed.
    async fn reauthorize(&self) -> Result<Never, Error> {
        loop {
            // Divide the reauthorization interval by 2 to avoid token expiration
            let delay = self.data.lock().await.reauthorize_every / 2;
            sleep(delay).await;
            let mut data = self.data.lock().await;
            let (access_token, reauthorize_every) = Self::authorize(&data.client_id, &data.client_secret, &self.client).await?;
            data.access_token = access_token;
            data.reauthorize_every = reauthorize_every;
        }
    }

    /// Retrieve current race information from the category detail API
    /// endpoint, retrieving the current race list. Creates a handler and task
    /// for any race that should be handled but currently isn't.
    ///
    /// This method runs in a constant loop, checking for new races every few
    /// seconds.
    async fn refresh_races<H: RaceHandler>(&self) -> Result<Never, Error> {
        loop {
            let data = match async { http_uri(&format!("/{}/data", self.data.lock().await.category_slug)) }
                .and_then(|url| async { Ok(self.client.get(url).send().await?.error_for_status()?.json::<CategoryData>().await?) })
                .await
            {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("Fatal error when attempting to retrieve race data: {:?}", e);
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
                            eprintln!("Fatal error when attempting to retrieve summary data: {:?}", e);
                            sleep(SCAN_RACES_EVERY).await;
                            continue
                        }
                    };
                    if H::should_handle(&race_data)? {
                        let (handler, stream) = self.create_handler::<H>(race_data).await?;
                        data.handled_races.insert(name.to_owned());
                        drop(data);
                        let name = name.to_owned();
                        let data_clone = self.data.clone();
                        tokio::spawn(async move {
                            handler.handle(stream).await.expect("error in race handler");
                            data_clone.lock().await.handled_races.remove(&name);
                        });
                    }
                }
            }
            sleep(SCAN_RACES_EVERY).await;
        }
    }

    /// Run the bot. Requires an active [`tokio`] runtime.
    pub async fn run<H: RaceHandler>(&self) -> Result<Never, Error> {
        let reauthorize_clone = self.clone();
        let refresh_races_clone = self.clone();
        tokio::select! {
            res = tokio::spawn(async move { reauthorize_clone.reauthorize().await }) => res?,
            res = tokio::spawn(async move { refresh_races_clone.refresh_races::<H>().await }) => res?,
        }
    }
}

/// Generate a HTTP/HTTPS URI from the given URL path fragment.
fn http_uri(url: &str) -> Result<Url, Error> {
    uri("https", url)
}

/// Generate a URI from the given protocol and URL path fragment.
fn uri(proto: &str, url: &str) -> Result<Url, Error> {
    Ok(format!("{proto}://{host}{url}",
        proto=proto,
        host=RACETIME_HOST,
        url=url,
    ).parse()?)
}

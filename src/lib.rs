//! Utilities for creating chat bots for [racetime.gg](https://racetime.gg/).
//!
//! The main entry point is [`Bot::run`]. You can also create new race rooms using [`StartRace::start`].
//!
//! For documentation, see also <https://github.com/racetimeGG/racetime-app/wiki/Category-bots>.

#![deny(rust_2018_idioms, unused, unused_crate_dependencies, unused_import_braces, unused_qualifications, warnings)]
#![forbid(unsafe_code)]

use {
    std::{
        collections::BTreeMap,
        time::Duration,
    },
    collect_mac::collect,
    itertools::Itertools as _,
    lazy_regex::regex_captures,
    serde::Deserialize,
    url::Url,
};
pub use crate::{
    bot::Bot,
    handler::RaceHandler,
};

pub mod bot;
pub mod handler;
pub mod model;

const RACETIME_HOST: &str = "racetime.gg";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)] Custom(#[from] Box<dyn std::error::Error + Send>),
    #[error(transparent)] HeaderToStr(#[from] reqwest::header::ToStrError),
    #[error(transparent)] InvalidHeaderValue(#[from] http::header::InvalidHeaderValue),
    #[error(transparent)] Io(#[from] std::io::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Task(#[from] tokio::task::JoinError),
    #[error(transparent)] UrlParse(#[from] url::ParseError),
    #[error("websocket connection closed by the server")]
    EndOfStream,
    #[error("the startrace location did not match the input category")]
    LocationCategory,
    #[error("the startrace location header did not have the expected format")]
    LocationFormat,
    #[error("the startrace response did not include a location header")]
    MissingLocationHeader,
    #[error("HTTP error{}: {0}", if let Some(url) = .0.url() { format!(" at {url}") } else { String::default() })]
    Reqwest(#[from] reqwest::Error),
    #[error("server errors:{}", .0.into_iter().map(|msg| format!("\n• {msg}")).format(""))]
    Server(Vec<String>),
    #[error("WebSocket error: {0}")]
    Tungstenite(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("expected text message from websocket, but received {0:?}")]
    UnexpectedMessageType(tokio_tungstenite::tungstenite::Message),
}

/// A convenience trait for converting results to use this crate's [`Error`] type.
pub trait ResultExt {
    type Ok;

    /// Convert the error to this crate's [`Error`] type using the [`Error::Custom`] variant.
    fn to_racetime(self) -> Result<Self::Ok, Error>;
}

impl<T, E: std::error::Error + Send + 'static> ResultExt for Result<T, E> {
    type Ok = T;

    fn to_racetime(self) -> Result<T, Error> {
        self.map_err(|e| Error::Custom(Box::new(e)))
    }
}

/// Generate a HTTP/HTTPS URI from the given URL path fragment.
fn http_uri(host: &str, url: &str) -> Result<Url, Error> {
    uri("https", host, url)
}

/// Generate a URI from the given protocol and URL path fragment.
fn uri(proto: &str, host: &str, url: &str) -> Result<Url, Error> {
    Ok(format!("{proto}://{host}{url}").parse()?)
}

/// Get an OAuth2 token from the authentication server.
pub async fn authorize(client_id: &str, client_secret: &str, client: &reqwest::Client) -> Result<(String, Duration), Error> {
    authorize_with_host(RACETIME_HOST, client_id, client_secret, client).await
}

pub async fn authorize_with_host(host: &str, client_id: &str, client_secret: &str, client: &reqwest::Client) -> Result<(String, Duration), Error> {
    #[derive(Deserialize)]
    struct AuthResponse {
        access_token: String,
        expires_in: Option<u64>,
    }

    let data = client.post(http_uri(host, "/o/token")?)
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

fn form_bool(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

pub struct StartRace {
    pub goal: String,
    pub goal_is_custom: bool,
    pub team_race: bool,
    pub invitational: bool,
    pub unlisted: bool,
    pub info_user: String,
    pub info_bot: String,
    pub require_even_teams: bool,
    /// Number of seconds the countdown should run for. Must be in `10..=60`.
    pub start_delay: u8,
    /// Maximum number of hours the race is allowed to run for. Must be in `1..=72`.
    pub time_limit: u8,
    pub time_limit_auto_complete: bool,
    pub streaming_required: Option<bool>,
    pub auto_start: bool,
    pub allow_comments: bool,
    pub hide_comments: bool,
    pub allow_prerace_chat: bool,
    pub allow_midrace_chat: bool,
    pub allow_non_entrant_chat: bool,
    /// Number of seconds to hold a message for before displaying it. Doesn't affect race monitors or moderators. Must be in `0..=90`.
    pub chat_message_delay: u8,
}

impl StartRace {
    /// Creates a race room with the specified configuration and returns its slug.
    ///
    /// An access token can be obtained using [`authorize`].
    pub async fn start(&self, access_token: &str, client: &reqwest::Client, category: &str) -> Result<String, Error> {
        self.start_with_host(RACETIME_HOST, access_token, client, category).await
    }

    pub async fn start_with_host(&self, host: &str, access_token: &str, client: &reqwest::Client, category: &str) -> Result<String, Error> {
        let start_delay = self.start_delay.to_string();
        let time_limit = self.time_limit.to_string();
        let chat_message_delay = self.chat_message_delay.to_string();
        let mut form = collect![as BTreeMap<_, _>:
            if self.goal_is_custom { "custom_goal" } else { "goal" } => &*self.goal,
            "team_race" => form_bool(self.team_race),
            "invitational" => form_bool(self.invitational),
            "unlisted" => form_bool(self.unlisted),
            "info_user" => &*self.info_user,
            "info_bot" => &*self.info_bot,
            "require_even_teams" => form_bool(self.require_even_teams),
            "start_delay" => &*start_delay,
            "time_limit" => &*time_limit,
            "time_limit_auto_complete" => form_bool(self.time_limit_auto_complete),
            "auto_start" => form_bool(self.auto_start),
            "allow_comments" => form_bool(self.allow_comments),
            "hide_comments" => form_bool(self.hide_comments),
            "allow_prerace_chat" => form_bool(self.allow_prerace_chat),
            "allow_midrace_chat" => form_bool(self.allow_midrace_chat),
            "allow_non_entrant_chat" => form_bool(self.allow_non_entrant_chat),
            "chat_message_delay" => &*chat_message_delay,
        ];
        if let Some(streaming_required) = self.streaming_required {
            form.insert("streaming_required", form_bool(streaming_required));
        }
        let response = client.post(http_uri(host, &format!("/o/{category}/startrace"))?)
            .bearer_auth(access_token)
            .form(&form)
            .send().await?
            .error_for_status()?;
        let location = response
            .headers()
            .get("location").ok_or(Error::MissingLocationHeader)?
            .to_str()?;
        let (_, location_category, slug) = regex_captures!("^/([^/]+)/([^/]+)$", location).ok_or(Error::LocationFormat)?;
        if location_category != category { return Err(Error::LocationCategory) }
        Ok(slug.to_owned())
    }
}

pub struct EditRace {
    // required fields

    /// If the race has already started, this must match the current goal.
    pub goal: String,
    /// If the race has already started, this must match the current goal.
    pub goal_is_custom: bool,
    /// Number of seconds the countdown should run for. Must be in `10..=60`.
    /// If the race has already started, this must match the current delay.
    pub start_delay: u8,
    /// Maximum number of hours the race is allowed to run for. Must be in `1..=72`.
    /// If the race has already started, this must match the current limit.
    pub time_limit: u8,
    /// Number of seconds to hold a message for before displaying it. Doesn't affect race monitors or moderators. Must be in `0..=90`.
    pub chat_message_delay: u8,

    // optional fields

    pub team_race: Option<bool>,
    pub unlisted: Option<bool>,
    pub info_user: Option<String>,
    pub info_bot: Option<String>,
    pub require_even_teams: Option<bool>,
    pub time_limit_auto_complete: Option<bool>,
    /// If the race has already started, this cannot be changed.
    pub streaming_required: Option<bool>,
    /// If the race has already started, this cannot be changed.
    pub auto_start: Option<bool>,
    pub allow_comments: Option<bool>,
    pub hide_comments: Option<bool>,
    pub allow_prerace_chat: Option<bool>,
    pub allow_midrace_chat: Option<bool>,
    pub allow_non_entrant_chat: Option<bool>,
}

impl EditRace {
    /// Edits the given race room, changing the specified fields.
    ///
    /// An access token can be obtained using [`authorize`].
    pub async fn edit(&self, access_token: &str, client: &reqwest::Client, category: &str, race_slug: &str) -> Result<(), Error> {
        self.edit_with_host(RACETIME_HOST, access_token, client, category, race_slug).await
    }

    pub async fn edit_with_host(&self, host: &str, access_token: &str, client: &reqwest::Client, category: &str, race_slug: &str) -> Result<(), Error> {
        let start_delay = self.start_delay.to_string();
        let time_limit = self.time_limit.to_string();
        let chat_message_delay = self.chat_message_delay.to_string();
        let mut form = collect![as BTreeMap<_, _>:
            if self.goal_is_custom { "custom_goal" } else { "goal" } => &*self.goal,
            "start_delay" => &*start_delay,
            "time_limit" => &*time_limit,
            "chat_message_delay" => &*chat_message_delay,
        ];
        if let Some(team_race) = self.team_race {
            form.insert("team_race", form_bool(team_race));
        }
        if let Some(unlisted) = self.unlisted {
            form.insert("unlisted", form_bool(unlisted));
        }
        if let Some(ref info_user) = self.info_user {
            form.insert("info_user", &**info_user);
        }
        if let Some(ref info_bot) = self.info_bot {
            form.insert("info_bot", &**info_bot);
        }
        if let Some(require_even_teams) = self.require_even_teams {
            form.insert("require_even_teams", form_bool(require_even_teams));
        }
        if let Some(time_limit_auto_complete) = self.time_limit_auto_complete {
            form.insert("time_limit_auto_complete", form_bool(time_limit_auto_complete));
        }
        if let Some(streaming_required) = self.streaming_required {
            form.insert("streaming_required", form_bool(streaming_required));
        }
        if let Some(auto_start) = self.auto_start {
            form.insert("auto_start", form_bool(auto_start));
        }
        if let Some(allow_comments) = self.allow_comments {
            form.insert("allow_comments", form_bool(allow_comments));
        }
        if let Some(hide_comments) = self.hide_comments {
            form.insert("hide_comments", form_bool(hide_comments));
        }
        if let Some(allow_prerace_chat) = self.allow_prerace_chat {
            form.insert("allow_prerace_chat", form_bool(allow_prerace_chat));
        }
        if let Some(allow_midrace_chat) = self.allow_midrace_chat {
            form.insert("allow_midrace_chat", form_bool(allow_midrace_chat));
        }
        if let Some(allow_non_entrant_chat) = self.allow_non_entrant_chat {
            form.insert("allow_non_entrant_chat", form_bool(allow_non_entrant_chat));
        }
        client.post(http_uri(host, &format!("/o/{category}/{race_slug}/edit"))?)
            .bearer_auth(access_token)
            .form(&form)
            .send().await?
            .error_for_status()?;
        Ok(())
    }
}

//! Utilities for creating chat bots for [racetime.gg](https://racetime.gg/).
//!
//! The main entry point is [`Bot::run`]. You can also create new race rooms using [`StartRace::start`].
//!
//! For documentation, see also <https://github.com/racetimeGG/racetime-app/wiki/Category-bots>.

use {
    std::{
        borrow::Cow,
        collections::BTreeMap,
        num::NonZeroU16,
    },
    collect_mac::collect,
    lazy_regex::regex_captures,
    serde::Deserialize,
    tokio::net::ToSocketAddrs,
    url::Url,
};
pub use crate::{
    bot::Bot,
    builder::BotBuilder,
    handler::RaceHandler,
};

pub mod bot;
mod builder;
pub mod handler;
pub mod model;

const RACETIME_HOST: &str = "racetime.gg";

/// An unsigned duration. This is a reexport of [`std::time::Duration`].
pub type UDuration = std::time::Duration;

#[derive(Debug, Clone)]
pub struct HostInfo {
    pub hostname: Cow<'static, str>,
    pub port: NonZeroU16,
    pub secure: bool,
}

impl HostInfo {
    pub fn new(hostname: impl Into<Cow<'static, str>>, port: NonZeroU16, secure: bool) -> Self {
        Self {
            hostname: hostname.into(),
            secure, port,
        }
    }

    fn http_protocol(&self) -> &'static str {
        match self.secure {
            true => "https",
            false => "http",
        }
    }

    fn websocket_protocol(&self) -> &'static str {
        match self.secure {
            true => "wss",
            false => "ws",
        }
    }
    fn http_uri(&self, url: &str) -> Result<Url, url::ParseError>  {
        uri(self.http_protocol(), &self.hostname, self.port, url)
    }

    fn websocket_uri(&self, url: &str) -> Result<Url, url::ParseError> {
        uri(self.websocket_protocol(), &self.hostname, self.port, url)
    }

    fn websocket_socketaddrs(&self) -> impl ToSocketAddrs + '_ {
        (&*self.hostname, self.port.get())
    }
}

impl Default for HostInfo {
    /// Returns the host info for racetime.gg.
    fn default() -> Self {
        Self {
            hostname: Cow::Borrowed(RACETIME_HOST),
            port: NonZeroU16::new(443).unwrap(),
            secure: true,
        }
    }
}

/// Generate a URI from the given protocol and URL path fragment.
fn uri(proto: &str, host: &str, port: NonZeroU16, url: &str) -> Result<Url, url::ParseError> {
    Ok(format!("{proto}://{host}:{port}{url}").parse()?)
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)] Http(#[from] reqwest::Error),
    #[error(transparent)] Url(#[from] url::ParseError),
    #[error("{inner}, body:\n\n{}", .text.as_ref().map(|text| text.clone()).unwrap_or_else(|e| e.to_string()))]
    ResponseStatus {
        #[source]
        inner: reqwest::Error,
        headers: reqwest::header::HeaderMap,
        text: reqwest::Result<String>,
    },
}

/// Get an OAuth2 token from the authentication server.
pub async fn authorize(client_id: &str, client_secret: &str, client: &reqwest::Client) -> Result<(String, UDuration), AuthError> {
    authorize_with_host(&HostInfo::default(), client_id, client_secret, client).await
}

pub async fn authorize_with_host(host_info: &HostInfo, client_id: &str, client_secret: &str, client: &reqwest::Client) -> Result<(String, UDuration), AuthError> {
    #[derive(Deserialize)]
    struct AuthResponse {
        access_token: String,
        expires_in: Option<u64>,
    }

    let response = client.post(host_info.http_uri("/o/token")?)
        .form(&collect![as BTreeMap<_, _>:
            "client_id" => client_id,
            "client_secret" => client_secret,
            "grant_type" => "client_credentials",
        ])
        .send().await?;
    let data = match response.error_for_status_ref() {
        Ok(_) => response.json::<AuthResponse>().await?,
        Err(inner) => return Err(AuthError::ResponseStatus {
            headers: response.headers().clone(),
            text: response.text().await,
            inner,
        }),
    };
    Ok((
        data.access_token,
        UDuration::from_secs(data.expires_in.unwrap_or(36000)),
    ))
}

fn form_bool(value: &bool) -> Cow<'static, str> {
    Cow::Borrowed(if *value { "true" } else { "false" })
}

pub struct StartRace {
    /// If the race has already started, this must match the current goal.
    pub goal: String,
    /// If the race has already started, this must match the current goal.
    pub goal_is_custom: bool,
    pub team_race: bool,
    /// If editing the race, this must match the current state. Use [`RaceContext::set_invitational`](handler::RaceContext::set_invitational) or [`RaceContext::set_open`](handler::RaceContext::set_open) instead.
    pub invitational: bool,
    /// Bots always have permission to set this field.
    pub unlisted: bool,
    /// Only available if category is opted-in to the 1v1 ladder beta.
    pub partitionable: bool,
    /// Only available if category is opted-in to the anonymised races beta.
    pub hide_entrants: bool,
    pub ranked: bool,
    pub info_user: String,
    pub info_bot: String,
    pub require_even_teams: bool,
    /// Number of seconds the countdown should run for. Must be in `10..=60`.
    /// If the race has already started, this must match the current delay.
    pub start_delay: u8,
    /// Maximum number of hours the race is allowed to run for. Must be in `1..=72`.
    /// If the race has already started, this must match the current limit.
    pub time_limit: u8,
    pub time_limit_auto_complete: bool,
    /// Bots always have permission to set this field.
    ///
    /// If the race has already started, this cannot be changed.
    pub streaming_required: bool,
    /// If the race has already started, this cannot be changed.
    pub auto_start: bool,
    pub allow_comments: bool,
    pub hide_comments: bool,
    pub allow_prerace_chat: bool,
    pub allow_midrace_chat: bool,
    pub allow_non_entrant_chat: bool,
    /// Number of seconds to hold a message for before displaying it. Doesn't affect race monitors or moderators. Must be in `0..=90`.
    pub chat_message_delay: u8,
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)] HeaderToStr(#[from] reqwest::header::ToStrError),
    #[error(transparent)] Http(#[from] reqwest::Error),
    #[error(transparent)] Url(#[from] url::ParseError),
    #[error("the startrace location did not match the input category")]
    LocationCategory,
    #[error("the startrace location header did not have the expected format")]
    LocationFormat,
    #[error("the startrace response did not include a location header")]
    MissingLocationHeader,
    #[error("{inner}, body:\n\n{}", .text.as_ref().map(|text| text.clone()).unwrap_or_else(|e| e.to_string()))]
    ResponseStatus {
        #[source]
        inner: reqwest::Error,
        headers: reqwest::header::HeaderMap,
        text: reqwest::Result<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error(transparent)] Http(#[from] reqwest::Error),
    #[error(transparent)] Url(#[from] url::ParseError),
    #[error("{inner}, body:\n\n{}", .text.as_ref().map(|text| text.clone()).unwrap_or_else(|e| e.to_string()))]
    ResponseStatus {
        #[source]
        inner: reqwest::Error,
        headers: reqwest::header::HeaderMap,
        text: reqwest::Result<String>,
    },
}

impl StartRace {
    fn form(&self) -> BTreeMap<&'static str, Cow<'_, str>> {
        let Self {
            goal,
            goal_is_custom,
            team_race,
            invitational,
            unlisted,
            partitionable,
            hide_entrants,
            ranked,
            info_user,
            info_bot,
            require_even_teams,
            start_delay,
            time_limit,
            time_limit_auto_complete,
            streaming_required,
            auto_start,
            allow_comments,
            hide_comments,
            allow_prerace_chat,
            allow_midrace_chat,
            allow_non_entrant_chat,
            chat_message_delay,
        } = self;
        collect![
            if *goal_is_custom { "custom_goal" } else { "goal" } => Cow::Borrowed(&**goal),
            "team_race" => form_bool(team_race),
            "invitational" => form_bool(invitational),
            "unlisted" => form_bool(unlisted),
            "partitionable" => form_bool(partitionable),
            "hide_entrants" => form_bool(hide_entrants),
            "ranked" => form_bool(ranked),
            "info_user" => Cow::Borrowed(&**info_user),
            "info_bot" => Cow::Borrowed(&**info_bot),
            "require_even_teams" => form_bool(require_even_teams),
            "start_delay" => Cow::Owned(start_delay.to_string()),
            "time_limit" => Cow::Owned(time_limit.to_string()),
            "time_limit_auto_complete" => form_bool(time_limit_auto_complete),
            "streaming_required" => form_bool(streaming_required),
            "auto_start" => form_bool(auto_start),
            "allow_comments" => form_bool(allow_comments),
            "hide_comments" => form_bool(hide_comments),
            "allow_prerace_chat" => form_bool(allow_prerace_chat),
            "allow_midrace_chat" => form_bool(allow_midrace_chat),
            "allow_non_entrant_chat" => form_bool(allow_non_entrant_chat),
            "chat_message_delay" => Cow::Owned(chat_message_delay.to_string()),
        ]
    }

    /// Creates a race room with the specified configuration and returns its slug.
    ///
    /// An access token can be obtained using [`authorize`].
    pub async fn start(&self, access_token: &str, client: &reqwest::Client, category: &str) -> Result<String, StartError> {
        self.start_with_host(&HostInfo::default(), access_token, client, category).await
    }

    pub async fn start_with_host(&self, host_info: &HostInfo, access_token: &str, client: &reqwest::Client, category: &str) -> Result<String, StartError> {
        let response = client.post(host_info.http_uri(&format!("/o/{category}/startrace"))?)
            .bearer_auth(access_token)
            .form(&self.form())
            .send().await?;
        if let Err(inner) = response.error_for_status_ref() {
            return Err(StartError::ResponseStatus {
                headers: response.headers().clone(),
                text: response.text().await,
                inner,
            })
        }
        let location = response
            .headers()
            .get("location").ok_or(StartError::MissingLocationHeader)?
            .to_str()?;
        let (_, location_category, slug) = regex_captures!("^/([^/]+)/([^/]+)$", location).ok_or(StartError::LocationFormat)?;
        if location_category != category { return Err(StartError::LocationCategory) }
        Ok(slug.to_owned())
    }

    /// Edits the given race room.
    ///
    /// Due to a limitation of the racetime.gg API, all fields including ones that should remain the same must be specified.
    ///
    /// An access token can be obtained using [`authorize`].
    pub async fn edit(&self, access_token: &str, client: &reqwest::Client, category: &str, race_slug: &str) -> Result<(), EditError> {
        self.edit_with_host(&HostInfo::default(), access_token, client, category, race_slug).await
    }

    pub async fn edit_with_host(&self, host_info: &HostInfo, access_token: &str, client: &reqwest::Client, category: &str, race_slug: &str) -> Result<(), EditError> {
        let response = client.post(host_info.http_uri(&format!("/o/{category}/{race_slug}/edit"))?)
            .bearer_auth(access_token)
            .form(&self.form())
            .send().await?;
        match response.error_for_status_ref() {
            Ok(_) => Ok(()),
            Err(inner) => Err(EditError::ResponseStatus {
                headers: response.headers().clone(),
                text: response.text().await,
                inner,
            }),
        }
    }
}

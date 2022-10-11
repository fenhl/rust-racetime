use {
    std::collections::BTreeMap,
    chrono::{
        Duration,
        prelude::*,
    },
    lazy_regex::regex_captures,
    serde::{
        Deserialize,
        de::{
            Deserializer,
            Error as _,
            Unexpected,
        },
    },
    url::Url,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type")]
pub enum Message {
    #[serde(rename = "chat.history")]
    ChatHistory {
        messages: Vec<ChatMessage>,
    },
    #[serde(rename = "chat.message")]
    ChatMessage {
        message: ChatMessage,
    },
    #[serde(rename = "chat.delete")]
    ChatDelete {
        delete: ChatDelete,
    },
    #[serde(rename = "chat.purge")]
    ChatPurge {
        purge: ChatPurge,
    },
    #[serde(rename = "error")]
    Error {
        errors: Vec<String>,
    },
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "race.data")]
    RaceData {
        race: RaceData,
    },
    #[serde(rename = "race.renders")]
    RaceRenders,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CategoryData {
    pub name: String,
    pub short_name: String,
    pub slug: String,
    pub url: String,
    pub data_url: String,
    pub image: Option<Url>,
    pub info: Option<String>,
    pub streaming_required: bool,
    pub owners: Vec<UserData>,
    pub moderators: Vec<UserData>,
    pub goals: Vec<String>,
    pub current_races: Vec<RaceSummary>,
    pub emotes: BTreeMap<String, Url>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CategorySummary {
    pub name: String,
    pub short_name: String,
    pub slug: String,
    pub url: String,
    pub data_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChatDelete {
    pub id: String,
    pub user: Option<UserData>,
    pub bot: Option<String>,
    pub is_bot: bool,
    pub deleted_by: UserData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    pub user: Option<UserData>,
    pub bot: Option<String>,
    pub posted_at: DateTime<Utc>,
    pub message: String,
    pub message_plain: String,
    pub highlight: bool,
    pub is_bot: bool,
    pub is_system: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ChatPurge {
    pub user: UserData,
    pub purged_by: UserData,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Goal {
    pub name: String,
    pub custom: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entrant {
    pub user: UserData,
    pub status: EntrantStatus,
    #[serde(deserialize_with = "deserialize_opt_django_duration")]
    pub finish_time: Option<Duration>,
    pub finished_at: Option<DateTime<Utc>>,
    pub place: Option<u32>,
    pub place_ordinal: Option<String>,
    pub score: Option<u32>,
    pub score_change: Option<i32>,
    pub comment: Option<String>,
    pub has_comment: bool,
    pub stream_live: bool,
    pub stream_override: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EntrantStatus {
    pub value: EntrantStatusValue,
    pub verbose_value: String,
    pub help_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntrantStatusValue {
    Requested,
    Invited,
    Declined,
    Ready,
    NotReady,
    InProgress,
    Done,
    Dnf,
    Dq,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RaceData {
    pub version: u32,
    pub name: String,
    pub category: CategorySummary,
    pub status: RaceStatus,
    pub url: String,
    pub data_url: String,
    pub websocket_url: String,
    pub websocket_bot_url: String,
    pub websocket_oauth_url: String,
    pub goal: Goal,
    pub info: String,
    pub info_bot: Option<String>,
    pub info_user: Option<String>,
    pub entrants_count: u32,
    pub entrants_count_finished: u32,
    pub entrants_count_inactive: u32,
    pub entrants: Vec<Entrant>,
    pub opened_at: DateTime<Utc>,
    #[serde(deserialize_with = "deserialize_django_duration")]
    pub start_delay: Duration,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub unlisted: bool,
    #[serde(deserialize_with = "deserialize_django_duration")]
    pub time_limit: Duration,
    pub streaming_required: bool,
    pub auto_start: bool,
    pub opened_by: Option<UserData>,
    pub monitors: Vec<UserData>,
    pub recordable: bool,
    pub recorded: bool,
    pub recorded_by: Option<UserData>,
    pub allow_comments: bool,
    pub hide_comments: bool,
    pub allow_midrace_chat: bool,
    pub allow_non_entrant_chat: bool,
    #[serde(deserialize_with = "deserialize_django_duration")]
    pub chat_message_delay: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RaceStatus {
    pub value: RaceStatusValue,
    pub verbose_value: String,
    pub help_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaceStatusValue {
    Open,
    Invitational,
    Pending,
    InProgress,
    Finished,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RaceSummary {
    pub name: String,
    pub category: Option<CategorySummary>,
    pub status: RaceStatus,
    pub url: String,
    pub data_url: String,
    pub goal: Goal,
    pub info: String,
    pub entrants_count: u32,
    pub entrants_count_finished: u32,
    pub entrants_count_inactive: u32,
    pub opened_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    #[serde(deserialize_with = "deserialize_django_duration")]
    pub time_limit: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UserData {
    pub id: String,
    pub full_name: String,
    pub name: String,
    pub discriminator: Option<String>,
    pub url: String,
    pub avatar: Option<String>,
    pub pronouns: Option<String>,
    pub flair: String,
    pub twitch_name: Option<String>,
    pub twitch_display_name: Option<String>,
    pub twitch_channel: Option<Url>,
    pub can_moderate: bool,
}

fn deserialize_django_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
    let s = String::deserialize(deserializer)?;
    if let Some((_, sign, days, hours, minutes, seconds, ms)) = regex_captures!("^(-?)P([0-9]+)DT([0-9]+)H([0-9]+)M([0-9]+)(?:\\.([0-9]+))?S$", &s) {
        Ok((Duration::days(days.parse().map_err(D::Error::custom)?)
        + Duration::hours(hours.parse().map_err(D::Error::custom)?)
        + Duration::minutes(minutes.parse().map_err(D::Error::custom)?)
        + Duration::seconds(seconds.parse().map_err(D::Error::custom)?)
        + Duration::microseconds(if ms.is_empty() { 0 } else { ms.parse().map_err(D::Error::custom)? }))
        * if sign == "-" { -1 } else { 1 })
    } else {
        Err(D::Error::invalid_value(Unexpected::Str(&s), &"a duration as produced by Django"))
    }
}

fn deserialize_opt_django_duration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
    if let Some(s) = Option::<String>::deserialize(deserializer)? {
        if let Some((_, sign, days, hours, minutes, seconds, ms)) = regex_captures!("^(-?)P([0-9]+)DT([0-9]+)H([0-9]+)M([0-9]+)(?:\\.([0-9]+))?S$", &s) {
            Ok(Some((Duration::days(days.parse().map_err(D::Error::custom)?)
            + Duration::hours(hours.parse().map_err(D::Error::custom)?)
            + Duration::minutes(minutes.parse().map_err(D::Error::custom)?)
            + Duration::seconds(seconds.parse().map_err(D::Error::custom)?)
            + Duration::microseconds(if ms.is_empty() { 0 } else { ms.parse().map_err(D::Error::custom)? }))
            * if sign == "-" { -1 } else { 1 }))
        } else {
            Err(D::Error::invalid_value(Unexpected::Str(&s), &"a duration as produced by Django"))
        }
    } else {
        Ok(None)
    }
}

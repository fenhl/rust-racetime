use {
    std::collections::BTreeMap,
    chrono::prelude::*,
    lazy_regex::regex_captures,
    serde::{
        Deserialize,
        Serialize,
        de::{
            Deserializer,
            Error as _,
            Unexpected,
        },
    },
    serde_with::{
        Map,
        serde_as,
    },
    url::Url,
    crate::UDuration,
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
    #[serde(rename = "chat.pin")]
    ChatPin {
        message: ChatMessage,
    },
    #[serde(rename = "chat.unpin")]
    ChatUnpin {
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
    #[serde(rename = "race.split")]
    RaceSplit,
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

#[serde_as]
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
    pub is_pinned: bool,
    #[serde_as(as = "Map<_, _>")]
    pub actions: Vec<(String, ActionButton)>,
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
    #[serde(deserialize_with = "deserialize_opt_django_uduration")]
    pub finish_time: Option<UDuration>,
    pub finished_at: Option<DateTime<Utc>>,
    pub place: Option<u32>,
    pub place_ordinal: Option<String>,
    pub score: Option<u32>,
    pub score_change: Option<i32>,
    pub comment: Option<String>,
    pub has_comment: bool,
    pub stream_live: bool,
    pub stream_override: bool,
    pub team: Option<EntrantTeam>,
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
pub struct EntrantTeam {
    pub name: String,
    pub slug: String,
    pub formal: bool,
    pub url: Option<String>,
    pub data_url: Option<String>,
    pub avatar: Option<Url>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RaceData {
    pub version: u32,
    pub name: String,
    pub slug: String,
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
    #[serde(deserialize_with = "deserialize_django_uduration")]
    pub start_delay: UDuration,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub unlisted: bool,
    #[serde(deserialize_with = "deserialize_django_uduration")]
    pub time_limit: UDuration,
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
    #[serde(deserialize_with = "deserialize_django_uduration")]
    pub chat_message_delay: UDuration,
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
    #[serde(deserialize_with = "deserialize_django_uduration")]
    pub time_limit: UDuration,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ActionButton {
    Message {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        help_text: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        survey: Option<Vec<SurveyQuestion>>, //TODO remove Option wrapping? (test whether an empty survey is different from no survey)
        #[serde(skip_serializing_if = "Option::is_none")]
        submit: Option<String>,
    },
    Url {
        url: Url,
        #[serde(skip_serializing_if = "Option::is_none")]
        help_text: Option<String>,
    }
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SurveyQuestion {
    pub name: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help_text: Option<String>,
    #[serde(rename = "type")]
    pub kind: SurveyQuestionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    #[serde_as(as = "Map<_, _>")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum SurveyQuestionKind {
    Input,
    Bool,
    Radio,
    Select,
}

fn deserialize_django_uduration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<UDuration, D::Error> {
    let s = String::deserialize(deserializer)?;
    if let Some((_, days, hours, minutes, seconds, ms)) = regex_captures!("^P([0-9]+)DT([0-9]+)H([0-9]+)M([0-9]+)(?:\\.([0-9]+))?S$", &s) {
        Ok(UDuration::from_secs(days.parse::<u64>().map_err(D::Error::custom)? * 60 * 60 * 24)
        + UDuration::from_secs(hours.parse::<u64>().map_err(D::Error::custom)? * 60 * 60)
        + UDuration::from_secs(minutes.parse::<u64>().map_err(D::Error::custom)? * 60)
        + UDuration::from_secs(seconds.parse().map_err(D::Error::custom)?)
        + UDuration::from_micros(if ms.is_empty() { 0 } else { ms.parse().map_err(D::Error::custom)? }))
    } else {
        Err(D::Error::invalid_value(Unexpected::Str(&s), &"a duration as produced by Django"))
    }
}

fn deserialize_opt_django_uduration<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<UDuration>, D::Error> {
    if let Some(s) = Option::<String>::deserialize(deserializer)? {
        if let Some((_, days, hours, minutes, seconds, ms)) = regex_captures!("^P([0-9]+)DT([0-9]+)H([0-9]+)M([0-9]+)(?:\\.([0-9]+))?S$", &s) {
            Ok(Some(UDuration::from_secs(days.parse::<u64>().map_err(D::Error::custom)? * 60 * 60 * 24)
            + UDuration::from_secs(hours.parse::<u64>().map_err(D::Error::custom)? * 60 * 60)
            + UDuration::from_secs(minutes.parse::<u64>().map_err(D::Error::custom)? * 60)
            + UDuration::from_secs(seconds.parse().map_err(D::Error::custom)?)
            + UDuration::from_micros(if ms.is_empty() { 0 } else { ms.parse().map_err(D::Error::custom)? })))
        } else {
            Err(D::Error::invalid_value(Unexpected::Str(&s), &"a duration as produced by Django"))
        }
    } else {
        Ok(None)
    }
}

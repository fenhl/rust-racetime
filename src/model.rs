use {
    serde::Deserialize,
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
    pub posted_at: String, //TODO DateTime<???>
    pub message: String,
    pub message_plain: String,
    pub highlight: bool,
    pub is_bot: bool,
    pub is_system: bool,
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
    pub finish_time: Option<String>, //TODO Option<Duration>
    pub finished_at: Option<String>, //TODO Option<DateTime<???>>
    pub place: u32,
    pub place_ordinal: String,
    pub score: u32,
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
    pub entrants_count: u32,
    pub entrants_count_finished: u32,
    pub entrants_count_inactive: u32,
    pub entrants: Vec<Entrant>,
    pub opened_at: String, //TODO DateTime<???>
    pub start_delay: String, //TODO Duration
    pub started_at: Option<String>, //TODO Option<DateTime<???>>
    pub ended_at: Option<String>, //TODO Option<DateTime<???>>
    pub cancelled_at: Option<String>, //TODO Option<DateTime<???>>
    pub unlisted: bool,
    pub time_limit: String, //TODO Duration
    pub streaming_required: bool,
    pub auto_start: bool,
    pub opened_by: Option<UserData>,
    pub monitors: Vec<UserData>,
    pub recordable: bool,
    pub recorded: bool,
    pub recorded_by: UserData,
    pub allow_comments: bool,
    pub hide_comments: bool,
    pub allow_midrace_chat: bool,
    pub allow_non_entrant_chat: bool,
    pub chat_message_delay: String, //TODO Duration
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
    pub twitch_channel: Option<Url>,
    pub can_moderate: bool,
}

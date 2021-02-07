use {
    std::{
        fmt,
        io,
    },
    async_trait::async_trait,
    derive_more::From,
    futures::{
        SinkExt,
        stream::{
            StreamExt as _,
            SplitSink,
            SplitStream,
        },
    },
    itertools::Itertools as _,
    serde_json::{
        Value as Json,
        json,
    },
    tokio::net::TcpStream,
    tokio_tungstenite::{
        WebSocketStream,
        tungstenite,
    },
    uuid::Uuid,
    crate::model::*,
};

pub type WsStream = SplitStream<WebSocketStream<TcpStream>>;
pub type WsSink = SplitSink<WebSocketStream<TcpStream>, tungstenite::Message>;

#[derive(Debug, From)]
pub enum Error {
    Connection(tungstenite::Error),
    DataUnimplemented,
    EndOfStream,
    Io(io::Error),
    Json(serde_json::Error),
    Other(String),
    SenderUnimplemented,
    Server(Vec<String>),
    UnexpectedMessageType(tungstenite::Message),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Connection(e) => write!(f, "websocket error: {}", e),
            Error::DataUnimplemented => write!(f, "this handler needs to implement `data` to be able to prepend/append to the race info"),
            Error::EndOfStream => write!(f, "websocket connection closed by the server"),
            Error::Io(e) => write!(f, "I/O error: {}", e),
            Error::Json(e) => write!(f, "JSON error: {}", e),
            Error::Other(msg) => msg.fmt(f),
            Error::SenderUnimplemented => write!(f, "this handler needs to implement `sender` to be able to send messages"),
            Error::Server(errors) => write!(f, "server errors:{}", errors.into_iter().map(|msg| format!("\n• {}", msg)).join("")),
            Error::UnexpectedMessageType(msg) => write!(f, "expected text message from websocket, but received {:?}", msg),
        }
    }
}

#[async_trait]
pub trait RaceHandler: Send + Sized + 'static {
    /// Called when a new race room is found and [`should_handle`](RaceHandler::should_handle) has returned [`true`].
    ///
    /// The `RaceHandler` this returns will receive events for that race.
    fn new(race_data: RaceData, sender: WsSink) -> Result<Self, Error>;

    /// Called when a new race room is found. If this returns [`false`], that race is ignored entirely.
    ///
    /// The default implementation returns [`true`] for any race whose status value is neither [`finished`](RaceStatusValue::Finished) nor [`cancelled`](RaceStatusValue::Cancelled).
    fn should_handle(race_data: &RaceData) -> Result<bool, Error> {
        Ok(!matches!(race_data.status.value, RaceStatusValue::Finished | RaceStatusValue::Cancelled))
    }

    /// Returns a mutable reference to the most recently received race data (via [`race_data`](RaceHandler::race_data) or [`new`](RaceHandler::new)).
    ///
    /// The default implementation returns an error, so this must be overwritten so that `race.data` messages are handled correctly and to be able to use the method [`set_raceinfo`](RaceHandler::set_raceinfo).
    fn data(&mut self) -> Result<&mut RaceData, Error> { Err(Error::DataUnimplemented) }

    /// Returns a mutable reference to the `sender` passed to [`new`](RaceHandler::new).
    ///
    /// The default implementation returns an error, so this must be overwritten to be able to use the methods [`send_raw`](RaceHandler::send_raw) and [`set_raceinfo`](RaceHandler::set_raceinfo) through [`remove_monitor`](RaceHandler::remove_monitor).
    fn sender(&mut self) -> Result<&mut WsSink, Error> { Err(Error::SenderUnimplemented) }

    /// Sends a raw JSON message to the server.
    ///
    /// The methods [`set_raceinfo`](RaceHandler::set_raceinfo) through [`remove_monitor`](RaceHandler::remove_monitor) should be preferred.
    ///
    /// Will always error unless `sender` is overwritten.
    async fn send_raw(&mut self, message: &Json) -> Result<(), Error> {
        self.sender()?.send(tungstenite::Message::Text(serde_json::to_string(&message)?)).await?;
        Ok(())
    }

    /// Called for each chat message that starts with `!` and was not sent by the system or a bot.
    async fn command(&mut self, _cmd_name: &str, _args: Vec<&str>, _is_moderator: bool, _is_monitor: bool, _: &ChatMessage) -> Result<(), Error> {
        Ok(())
    }

    /// Determine if the handler should be terminated. This is checked after
    /// every receieved message.
    ///
    /// The default implementation checks [`should_handle`](RaceHandler::should_handle).
    fn should_stop(&mut self) -> Result<bool, Error> {
        Ok(match self.data() {
            Ok(data) => !Self::should_handle(data)?,
            Err(Error::DataUnimplemented) => false,
            Err(e) => return Err(e),
        })
    }

    /// Bot actions to perform when first connecting to a race room.
    ///
    /// The default implementation does nothing.
    async fn begin(&mut self) -> Result<(), Error> { Ok(()) }

    /// Standard message consumer. This is called for every message we receive
    /// from the site.
    ///
    /// The default implementation will simply call the appropriate method
    /// to handle the incoming data, based on its type. For example if we have
    /// a "race.data" type message, it will call `self.race_data(data)`.
    async fn consume(&mut self, data: Message) -> Result<(), Error> {
        match data {
            Message::ChatHistory { messages } => self.chat_history(messages).await,
            Message::ChatMessage { message } => self.chat_message(message).await,
            Message::ChatDelete { delete } => self.chat_delete(delete).await,
            Message::ChatPurge { purge } => self.chat_purge(purge).await,
            Message::Error { errors } => self.error(errors).await,
            Message::Pong => self.pong().await,
            Message::RaceData { race } => self.race_data(race).await,
        }
    }

    /// Bot actions to perform just before disconnecting from a race room.
    ///
    /// The default implementation does nothing.
    async fn end(self) -> Result<(), Error> { Ok(()) }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when a `chat.history`
    /// message is received.
    ///
    /// The default implementation does nothing.
    async fn chat_history(&mut self, _: Vec<ChatMessage>) -> Result<(), Error> { Ok(()) }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when a `chat.message`
    /// message is received.
    ///
    /// The default implementation calls [`command`](RaceHandler::command) if appropriate.
    async fn chat_message(&mut self, message: ChatMessage) -> Result<(), Error> {
        if !message.is_bot && !message.is_system && message.message.starts_with('!') {
            let can_monitor = match self.data() {
                Ok(data) => message.user.as_ref().map_or(false, |sender|
                    data.opened_by.as_ref().map_or(false, |creator| creator.id == sender.id) || data.monitors.iter().any(|monitor| monitor.id == sender.id)
                ),
                Err(Error::DataUnimplemented) => false,
                Err(e) => return Err(e),
            };
            let mut split = message.message[1..].split(' ');
            self.command(
                split.next().expect("split always yields at least one item"),
                split.collect(),
                message.user.as_ref().map_or(false, |user| user.can_moderate),
                can_monitor,
                &message,
            ).await?;
        }
        Ok(())
    }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when a `chat.delete`
    /// message is received.
    ///
    /// The default implementation does nothing.
    async fn chat_delete(&mut self, _: ChatDelete) -> Result<(), Error> { Ok(()) }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when a `chat.purge`
    /// message is received.
    ///
    /// The default implementation does nothing.
    async fn chat_purge(&mut self, _: ChatPurge) -> Result<(), Error> { Ok(()) }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when an `error`
    /// message is received.
    ///
    /// The default implementation returns the errors as `Error::Server`.
    async fn error(&mut self, errors: Vec<String>) -> Result<(), Error> {
        Err(Error::Server(errors))
    }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when a `pong`
    /// message is received.
    ///
    /// The default implementation does nothing.
    async fn pong(&mut self) -> Result<(), Error> { Ok(()) }

    /// The default implementation of [`consume`](RaceHandler::consume) calls this when a `race.data`
    /// message is received.
    ///
    /// The default implementation tries to store the `race_data` using the [`data`](RaceHandler::data) method, but fails silently for that method's default implementation.
    async fn race_data(&mut self, race_data: RaceData) -> Result<(), Error> {
        match self.data() {
            Ok(data) => *data = race_data,
            Err(Error::DataUnimplemented) => {}
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Send a chat message to the race room.
    ///
    /// `message` should be the message string you want to send.
    async fn send_message(&mut self, message: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "message",
            "data": {
                "message": message,
                "guid": Uuid::new_v4(),
            }
        })).await?;
        Ok(())
    }

    /// Set the `info` field on the race room's data.
    ///
    /// `info` should be the information you wish to set, and `pos` the behavior in case there is existing info.
    async fn set_raceinfo(&mut self, info: &str, pos: RaceInfoPos) -> Result<(), Error> {
        let info = match (self.data().map(|data| &data.info[..]), pos) {
            (Ok(""), _) | (_, RaceInfoPos::Overwrite) => info.to_owned(),
            (Ok(old_info), RaceInfoPos::Prefix) => format!("{} | {}", info, old_info),
            (Ok(old_info), RaceInfoPos::Suffix) => format!("{} | {}", old_info, info),
            (Err(e), _) => return Err(e),
        };
        self.send_raw(&json!({
            "action": "setinfo",
            "data": {"info": info}
        })).await?;
        Ok(())
    }

    /// Set the room in an open state.
    async fn set_open(&mut self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "make_open"
        })).await?;
        Ok(())
    }

    /// Set the room in an invite-only state.
    async fn set_invitational(&mut self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "make_invitational"
        })).await?;
        Ok(())
    }

    /// Forces a start of the race.
    async fn force_start(&mut self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "begin"
        })).await?;
        Ok(())
    }

    /// Forcibly cancels a race.
    async fn cancel_race(&mut self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "cancel"
        })).await?;
        Ok(())
    }

    /// Invites a user to the race.
    ///
    /// `user` should be the hashid of the user.
    async fn invite_user(&mut self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "invite",
            "data": {
                "user": user
            }
        })).await?;
        Ok(())
    }

    /// Accepts a request to join the race room.
    ///
    /// `user` should be the hashid of the user.
    async fn accept_request(&mut self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "accept_request",
            "data": {
                "user": user
            }
        })).await?;
        Ok(())
    }

    /// Forcibly unreadies an entrant.
    ///
    /// `user` should be the hashid of the user.
    async fn force_unready(&mut self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "force_unready",
            "data": {
                "user": user
            }
        })).await?;
        Ok(())
    }

    /// Forcibly removes an entrant from the race.
    ///
    /// `user` should be the hashid of the user.
    async fn remove_entrant(&mut self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "remove_entrant",
            "data": {
                "user": user
            }
        })).await?;
        Ok(())
    }

    /// Adds a user as a race monitor.
    ///
    /// `user` should be the hashid of the user.
    async fn add_monitor(&mut self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "add_monitor",
            "data": {
                "user": user
            }
        })).await?;
        Ok(())
    }

    /// Removes a user as a race monitor.
    ///
    /// `user` should be the hashid of the user.
    async fn remove_monitor(&mut self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "remove_monitor",
            "data": {
                "user": user
            }
        })).await?;
        Ok(())
    }

    /// Low-level handler for the race room.
    ///
    /// The default implementation loops over the websocket,
    /// calling [`consume`](RaceHandler::consume) for each message that comes in.
    async fn handle(mut self, mut stream: WsStream) -> Result<(), Error> {
        self.begin().await?;
        while let Some(msg_res) = stream.next().await {
            match msg_res {
                Ok(tungstenite::Message::Text(buf)) => {
                    let data = serde_json::from_str(&buf)?;
                    self.consume(data).await?;
                    if self.should_stop()? {
                        return self.end().await
                    }
                }
                Ok(msg) => return Err(Error::UnexpectedMessageType(msg)),
                Err(e) => return Err(Error::Connection(e)),
            }
        }
        Err(Error::EndOfStream)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaceInfoPos {
    /// Replace the existing race info with the new one.
    Overwrite,
    /// Add the new race info before the existing one, if any, like so: `new | old`
    Prefix,
    /// Add the new race info after the existing one, if any, like so: `old | new`
    Suffix,
}

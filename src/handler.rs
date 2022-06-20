use {
    std::sync::Arc,
    async_trait::async_trait,
    futures::{
        SinkExt as _,
        stream::{
            SplitSink,
            SplitStream,
        },
    },
    serde_json::{
        Value as Json,
        json,
    },
    tokio::{
        net::TcpStream,
        sync::{
            Mutex,
            RwLock,
            RwLockReadGuard,
        },
    },
    tokio_tungstenite::{
        MaybeTlsStream,
        WebSocketStream,
        tungstenite,
    },
    uuid::Uuid,
    crate::{
        Error,
        model::*,
    },
};

pub type WsStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;
pub type WsSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, tungstenite::Message>;

/// A type passed to [`RaceHandler`] callback methods which can be used to check the current status of the race or send messages.
#[derive(Clone)]
pub struct RaceContext {
    pub(crate) data: Arc<RwLock<RaceData>>,
    pub sender: Arc<Mutex<WsSink>>,
}

impl RaceContext {
    /// Returns the current state of the race.
    pub async fn data(&self) -> RwLockReadGuard<'_, RaceData> {
        self.data.read().await
    }

    /// Sends a raw JSON message to the server.
    ///
    /// The methods [`set_raceinfo`](RaceContext::set_raceinfo) through [`remove_monitor`](RaceContext::remove_monitor) should be preferred.
    pub async fn send_raw(&self, message: &Json) -> Result<(), Error> {
        self.sender.lock().await.send(tungstenite::Message::Text(serde_json::to_string(&message)?)).await?;
        Ok(())
    }

    /// Send a chat message to the race room.
    ///
    /// `message` should be the message string you want to send.
    pub async fn send_message(&self, message: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "message",
            "data": {
                "message": message,
                "guid": Uuid::new_v4(),
            },
        })).await?;
        Ok(())
    }

    /// Set the `info` field on the race room's data.
    ///
    /// `info` should be the information you wish to set, and `pos` the behavior in case there is existing info.
    pub async fn set_raceinfo(&self, info: &str, pos: RaceInfoPos) -> Result<(), Error> {
        let info = match (&*self.data().await.info, pos) {
            ("", _) | (_, RaceInfoPos::Overwrite) => info.to_owned(),
            (old_info, RaceInfoPos::Prefix) => format!("{info} | {old_info}"),
            (old_info, RaceInfoPos::Suffix) => format!("{old_info} | {info}"),
        };
        self.send_raw(&json!({
            "action": "setinfo",
            "data": {"info": info},
        })).await?;
        Ok(())
    }

    /// Set the room in an open state.
    pub async fn set_open(&self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "make_open",
        })).await?;
        Ok(())
    }

    /// Set the room in an invite-only state.
    pub async fn set_invitational(&self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "make_invitational",
        })).await?;
        Ok(())
    }

    /// Forces a start of the race.
    pub async fn force_start(&self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "begin",
        })).await?;
        Ok(())
    }

    /// Forcibly cancels a race.
    pub async fn cancel_race(&self) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "cancel",
        })).await?;
        Ok(())
    }

    /// Invites a user to the race.
    ///
    /// `user` should be the hashid of the user.
    pub async fn invite_user(&self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "invite",
            "data": {
                "user": user,
            },
        })).await?;
        Ok(())
    }

    /// Accepts a request to join the race room.
    ///
    /// `user` should be the hashid of the user.
    pub async fn accept_request(&self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "accept_request",
            "data": {
                "user": user,
            },
        })).await?;
        Ok(())
    }

    /// Forcibly unreadies an entrant.
    ///
    /// `user` should be the hashid of the user.
    pub async fn force_unready(&self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "force_unready",
            "data": {
                "user": user,
            },
        })).await?;
        Ok(())
    }

    /// Forcibly removes an entrant from the race.
    ///
    /// `user` should be the hashid of the user.
    pub async fn remove_entrant(&self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "remove_entrant",
            "data": {
                "user": user,
            },
        })).await?;
        Ok(())
    }

    /// Adds a user as a race monitor.
    ///
    /// `user` should be the hashid of the user.
    pub async fn add_monitor(&self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "add_monitor",
            "data": {
                "user": user,
            },
        })).await?;
        Ok(())
    }

    /// Removes a user as a race monitor.
    ///
    /// `user` should be the hashid of the user.
    pub async fn remove_monitor(&self, user: &str) -> Result<(), Error> {
        self.send_raw(&json!({
            "action": "remove_monitor",
            "data": {
                "user": user,
            },
        })).await?;
        Ok(())
    }
}

/// This trait should be implemented using the [`async_trait`] attribute.
#[async_trait]
pub trait RaceHandler: Send + Sized + 'static {
    /// Called when a new race room is found. If this returns [`false`], that race is ignored entirely.
    ///
    /// The default implementation returns [`true`] for any race whose status value is neither [`finished`](RaceStatusValue::Finished) nor [`cancelled`](RaceStatusValue::Cancelled).
    fn should_handle(race_data: &RaceData) -> Result<bool, Error> {
        Ok(!matches!(race_data.status.value, RaceStatusValue::Finished | RaceStatusValue::Cancelled))
    }

    /// Called when a new race room is found and [`should_handle`](RaceHandler::should_handle) has returned [`true`].
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn new(ctx: &RaceContext) -> Result<Self, Error>;
    /// ```
    ///
    /// The `RaceHandler` this returns will receive events for that race.
    async fn new(ctx: &RaceContext) -> Result<Self, Error>;

    /// Called for each chat message that starts with `!` and was not sent by the system or a bot.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn command(&mut self: _ctx: &RaceContext, _cmd_name: &str, _args: Vec<&str>, _is_moderator: bool, _is_monitor: bool, _msg: &ChatMessage) -> Result<(), Error>;
    /// ```
    async fn command(&mut self, _ctx: &RaceContext, _cmd_name: &str, _args: Vec<&str>, _is_moderator: bool, _is_monitor: bool, _msg: &ChatMessage) -> Result<(), Error> {
        Ok(())
    }

    /// Determine if the handler should be terminated. This is checked after every receieved message.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn should_stop(&mut self, ctx: &RaceContext) -> Result<bool, Error>;
    /// ```
    ///
    /// The default implementation checks [`should_handle`](RaceHandler::should_handle).
    async fn should_stop(&mut self, ctx: &RaceContext) -> Result<bool, Error> {
        Ok(!Self::should_handle(&*ctx.data().await)?)
    }

    /// Bot actions to perform just before disconnecting from a race room.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn end(self, _ctx: &RaceContext) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn end(self, _ctx: &RaceContext) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.history` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_history(&mut self, _ctx: &RaceContext: _msgs: Vec<ChatMessage>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_history(&mut self, _ctx: &RaceContext, _msgs: Vec<ChatMessage>) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.message` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_message(&mut self, ctx: &RaceContext, message: ChatMessage) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation calls [`command`](RaceHandler::command) if appropriate.
    async fn chat_message(&mut self, ctx: &RaceContext, message: ChatMessage) -> Result<(), Error> {
        if !message.is_bot && !message.is_system.unwrap_or(false /* Python duck typing strikes again */) && message.message.starts_with('!') {
            let data = ctx.data().await;
            let can_monitor = message.user.as_ref().map_or(false, |sender|
                data.opened_by.as_ref().map_or(false, |creator| creator.id == sender.id) || data.monitors.iter().any(|monitor| monitor.id == sender.id)
            );
            let mut split = message.message[1..].split(' ');
            self.command(
                ctx,
                split.next().expect("split always yields at least one item"),
                split.collect(),
                message.user.as_ref().map_or(false, |user| user.can_moderate),
                can_monitor,
                &message,
            ).await?;
        }
        Ok(())
    }

    /// Called when a `chat.delete` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_delete(&mut self, _ctx: &RaceContext, _event: ChatDelete) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_delete(&mut self, _ctx: &RaceContext, _event: ChatDelete) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.purge` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_purge(&mut self, _ctx: &RaceContext, _event: ChatPurge) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_purge(&mut self, _ctx: &RaceContext, _event: ChatPurge) -> Result<(), Error> { Ok(()) }

    /// Called when an `error` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn error(&mut self, _ctx: &RaceContext, errors: Vec<String>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation returns the errors as `Error::Server`.
    async fn error(&mut self, _ctx: &RaceContext, errors: Vec<String>) -> Result<(), Error> {
        Err(Error::Server(errors))
    }

    /// Called when a `pong` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn pong(&mut self, _ctx: &RaceContext) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn pong(&mut self, _ctx: &RaceContext) -> Result<(), Error> { Ok(()) }

    /// Called when a `race.data` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn race_data(&mut self, _ctx: &RaceContext, _old_race_data: RaceData) -> Result<(), Error>;
    /// ```
    ///
    /// The new race data can be found in the [`RaceContext`] parameter. The [`RaceData`] parameter contains the previous data.
    ///
    /// The default implementation does nothing.
    async fn race_data(&mut self, _ctx: &RaceContext, _old_race_data: RaceData) -> Result<(), Error> { Ok(()) }

    /// Called when a `race.renders` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn race_renders(&mut self, _ctx: &RaceContext) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn race_renders(&mut self, _ctx: &RaceContext) -> Result<(), Error> { Ok(()) }
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

use {
    std::{
        borrow::Cow,
        fmt,
        sync::Arc,
    },
    async_trait::async_trait,
    futures::{
        SinkExt as _,
        stream::{
            SplitSink,
            SplitStream,
        },
    },
    serde::Serialize,
    serde_with::{
        DisplayFromStr,
        Map,
        serde_as,
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
///
/// While a future returned from a callback method is running, [`data`](Self::data) will not change and no other callbacks will be processed.
/// [`Clone`] can be used to keep a value of this type past the end of a handler callback, e.g. by passing it into [`tokio::spawn`].
/// If you do so, [`data`](Self::data) can be called to check the current state of the race.
pub struct RaceContext<S: Send + Sync + ?Sized + 'static> {
    pub global_state: Arc<S>,
    pub(crate) data: Arc<RwLock<RaceData>>,
    pub(crate) sender: Arc<Mutex<WsSink>>,
}

impl<S: Send + Sync + ?Sized + 'static> RaceContext<S> {
    /// Returns the current state of the race.
    ///
    /// See the type-level documentation for the semantics of using a [`RaceContext`] for an extended period of time.
    pub async fn data(&self) -> RwLockReadGuard<'_, RaceData> {
        self.data.read().await
    }

    async fn send_raw(&self, action: &'static str, data: impl Serialize) -> Result<(), Error> {
        #[derive(Serialize)]
        struct RawMessage<T> {
            action: &'static str,
            data: T,
        }

        self.sender.lock().await.send(tungstenite::Message::Text(serde_json::to_string(&RawMessage { action, data })?)).await?;
        Ok(())
    }

    /// Send a simple chat message to the race room.
    ///
    /// See [`send_message`](Self::send_message) for more options.
    pub async fn say(&self, message: impl fmt::Display) -> Result<(), Error> {
        #[serde_as]
        #[derive(Serialize)]
        struct Data<T: fmt::Display> {
            #[serde_as(as = "DisplayFromStr")]
            message: T,
            guid: Uuid,
        }

        self.send_raw("message", Data {
            guid: Uuid::new_v4(),
            message,
        }).await
    }

    /// Send a chat message to the race room.
    ///
    /// * `message` should be the message string you want to send.
    /// * If `pinned` is set to true, the sent message will be automatically pinned.
    /// * If `actions` is provided, action buttons will appear below your message for users to click on. See [action buttons](https://github.com/racetimeGG/racetime-app/wiki/Category-bots#action-buttons) for more details.
    pub async fn send_message(&self, message: impl fmt::Display, pinned: bool, actions: Vec<(&str, ActionButton)>) -> Result<(), Error> {
        #[serde_as]
        #[derive(Serialize)]
        struct Data<'a, T: fmt::Display> {
            #[serde_as(as = "DisplayFromStr")]
            message: T,
            pinned: bool,
            #[serde_as(as = "Map<_, _>")]
            actions: Vec<(&'a str, ActionButton)>,
            guid: Uuid,
        }

        self.send_raw("message", Data {
            guid: Uuid::new_v4(),
            message, pinned, actions,
        }).await
    }

    /// Send a direct message only visible to its recipient.
    ///
    /// * `message` should be the message string you want to send.
    /// * `direct_to` should be the hashid of the user.
    pub async fn send_direct_message(&self, message: &str, direct_to: &str) -> Result<(), Error> {
        #[serde_as]
        #[derive(Serialize)]
        struct Data<'a> {
            message: &'a str,
            direct_to: &'a str,
            guid: Uuid,
        }

        self.send_raw("message", Data {
            guid: Uuid::new_v4(),
            message, direct_to,
        }).await
    }

    /// Pin a chat message.
    ///
    /// `message_id` should be the `id` field of a [`ChatMessage`].
    pub async fn pin_message(&self, message_id: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            message: &'a str,
        }

        self.send_raw("pin_message", Data {
            message: message_id,
        }).await
    }

    /// Unpin a chat message.
    ///
    /// `message_id` should be the `id` field of a [`ChatMessage`].
    pub async fn unpin_message(&self, message_id: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            message: &'a str,
        }

        self.send_raw("unpin_message", Data {
            message: message_id,
        }).await
    }

    /// Set the `info_bot` field on the race room's data.
    pub async fn set_bot_raceinfo(&self, info: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            info_bot: &'a str,
        }

        self.send_raw("setinfo", Data {
            info_bot: info,
        }).await
    }

    /// Set the `info_user` field on the race room's data.
    ///
    /// `info` should be the information you wish to set, and `pos` the behavior in case there is existing info.
    pub async fn set_user_raceinfo(&self, info: &str, pos: RaceInfoPos) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            info_user: Cow<'a, str>,
        }

        let info_user = match (&*self.data().await.info, pos) {
            ("", _) | (_, RaceInfoPos::Overwrite) => Cow::Borrowed(info),
            (old_info, RaceInfoPos::Prefix) => Cow::Owned(format!("{info} | {old_info}")),
            (old_info, RaceInfoPos::Suffix) => Cow::Owned(format!("{old_info} | {info}")),
        };
        self.send_raw("setinfo", Data {
            info_user,
        }).await
    }

    /// Set the room in an open state.
    pub async fn set_open(&self) -> Result<(), Error> {
        self.sender.lock().await.send(tungstenite::Message::Text(format!("{{\"action\": \"make_open\"}}"))).await?;
        Ok(())
    }

    /// Set the room in an invite-only state.
    pub async fn set_invitational(&self) -> Result<(), Error> {
        self.sender.lock().await.send(tungstenite::Message::Text(format!("{{\"action\": \"make_invitational\"}}"))).await?;
        Ok(())
    }

    /// Forces a start of the race.
    pub async fn force_start(&self) -> Result<(), Error> {
        self.sender.lock().await.send(tungstenite::Message::Text(format!("{{\"action\": \"begin\"}}"))).await?;
        Ok(())
    }

    /// Forcibly cancels a race.
    pub async fn cancel_race(&self) -> Result<(), Error> {
        self.sender.lock().await.send(tungstenite::Message::Text(format!("{{\"action\": \"cancel\"}}"))).await?;
        Ok(())
    }

    /// Invites a user to the race.
    ///
    /// `user` should be the hashid of the user.
    pub async fn invite_user(&self, user: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            user: &'a str,
        }

        self.send_raw("invite", Data {
            user,
        }).await
    }

    /// Accepts a request to join the race room.
    ///
    /// `user` should be the hashid of the user.
    pub async fn accept_request(&self, user: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            user: &'a str,
        }

        self.send_raw("accept_request", Data {
            user,
        }).await
    }

    /// Forcibly unreadies an entrant.
    ///
    /// `user` should be the hashid of the user.
    pub async fn force_unready(&self, user: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            user: &'a str,
        }

        self.send_raw("force_unready", Data {
            user,
        }).await
    }

    /// Forcibly removes an entrant from the race.
    ///
    /// `user` should be the hashid of the user.
    pub async fn remove_entrant(&self, user: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            user: &'a str,
        }

        self.send_raw("remove_entrant", Data {
            user,
        }).await
    }

    /// Adds a user as a race monitor.
    ///
    /// `user` should be the hashid of the user.
    pub async fn add_monitor(&self, user: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            user: &'a str,
        }

        self.send_raw("add_monitor", Data {
            user,
        }).await
    }

    /// Removes a user as a race monitor.
    ///
    /// `user` should be the hashid of the user.
    pub async fn remove_monitor(&self, user: &str) -> Result<(), Error> {
        #[derive(Serialize)]
        struct Data<'a> {
            user: &'a str,
        }

        self.send_raw("remove_monitor", Data {
            user,
        }).await
    }
}

impl<S: Send + Sync + ?Sized + 'static> Clone for RaceContext<S> {
    /// This is a cheap operation since this type is a wrapper around some [`Arc`]s. See the type-level documentation for the semantics of using a [`RaceContext`] for an extended period of time.
    fn clone(&self) -> Self {
        Self {
            global_state: Arc::clone(&self.global_state),
            data: Arc::clone(&self.data),
            sender: Arc::clone(&self.sender),
        }
    }
}

/// This trait should be implemented using the [`macro@async_trait`] attribute.
#[async_trait] // required because Rust's built-in async trait methods don't generate `Send` bounds
pub trait RaceHandler<S: Send + Sync + ?Sized + 'static>: Send + Sized + 'static {
    /// Called when a new race room is found. If this returns [`false`], that race is ignored entirely.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn should_handle(race_data: &RaceData, _state: Arc<S>) -> Result<bool, Error>;
    /// ```
    ///
    /// The default implementation returns [`true`] for any race whose status value is neither [`finished`](RaceStatusValue::Finished) nor [`cancelled`](RaceStatusValue::Cancelled).
    async fn should_handle(race_data: &RaceData, _state: Arc<S>) -> Result<bool, Error> {
        Ok(!matches!(race_data.status.value, RaceStatusValue::Finished | RaceStatusValue::Cancelled))
    }

    /// Called when a new race room is found and [`should_handle`](RaceHandler::should_handle) has returned [`true`].
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn new(ctx: &RaceContext<S>) -> Result<Self, Error>;
    /// ```
    ///
    /// The `RaceHandler` this returns will receive events for that race.
    async fn new(ctx: &RaceContext<S>) -> Result<Self, Error>;

    /// Called for each chat message that starts with `!` and was not sent by the system or a bot.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn command(&mut self: _ctx: &RaceContext<S>, _cmd_name: String, _args: Vec<String>, _is_moderator: bool, _is_monitor: bool, _msg: &ChatMessage) -> Result<(), Error>;
    /// ```
    async fn command(&mut self, _ctx: &RaceContext<S>, _cmd_name: String, _args: Vec<String>, _is_moderator: bool, _is_monitor: bool, _msg: &ChatMessage) -> Result<(), Error> {
        Ok(())
    }

    /// Determine if the handler should be terminated. This is checked after every receieved message.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn should_stop(&mut self, ctx: &RaceContext<S>) -> Result<bool, Error>;
    /// ```
    ///
    /// The default implementation checks [`should_handle`](RaceHandler::should_handle).
    async fn should_stop(&mut self, ctx: &RaceContext<S>) -> Result<bool, Error> {
        Ok(!Self::should_handle(&*ctx.data().await, ctx.global_state.clone()).await?)
    }

    /// Bot actions to perform just before disconnecting from a race room.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn end(self, _ctx: &RaceContext<S>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn end(self, _ctx: &RaceContext<S>) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.history` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_history(&mut self, _ctx: &RaceContext<S>: _msgs: Vec<ChatMessage>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_history(&mut self, _ctx: &RaceContext<S>, _msgs: Vec<ChatMessage>) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.message` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_message(&mut self, ctx: &RaceContext<S>, message: ChatMessage) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation calls [`command`](RaceHandler::command) if appropriate.
    async fn chat_message(&mut self, ctx: &RaceContext<S>, message: ChatMessage) -> Result<(), Error> {
        if !message.is_bot && !message.is_system.unwrap_or(false /* Python duck typing strikes again */) && message.message.starts_with('!') {
            let data = ctx.data().await;
            let can_moderate = message.user.as_ref().map_or(false, |user| user.can_moderate);
            let can_monitor = can_moderate || message.user.as_ref().map_or(false, |sender|
                data.opened_by.as_ref().map_or(false, |creator| creator.id == sender.id) || data.monitors.iter().any(|monitor| monitor.id == sender.id)
            );
            if let Some(mut split) = shlex::split(&message.message[1..]) {
                if !split.is_empty() {
                    self.command(ctx, split.remove(0), split, can_moderate, can_monitor, &message).await?;
                }
            }
        }
        Ok(())
    }

    /// Called when a `chat.pin` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_pin(&mut self, _ctx: &RaceContext<S>, _message: ChatMessage) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_pin(&mut self, _ctx: &RaceContext<S>, _message: ChatMessage) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.unpin` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_unpin(&mut self, _ctx: &RaceContext<S>, _message: ChatMessage) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_unpin(&mut self, _ctx: &RaceContext<S>, _message: ChatMessage) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.delete` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_delete(&mut self, _ctx: &RaceContext<S>, _event: ChatDelete) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_delete(&mut self, _ctx: &RaceContext<S>, _event: ChatDelete) -> Result<(), Error> { Ok(()) }

    /// Called when a `chat.purge` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn chat_purge(&mut self, _ctx: &RaceContext<S>, _event: ChatPurge) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn chat_purge(&mut self, _ctx: &RaceContext<S>, _event: ChatPurge) -> Result<(), Error> { Ok(()) }

    /// Called when an `error` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn error(&mut self, _ctx: &RaceContext<S>, errors: Vec<String>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation returns the errors as `Error::Server`.
    async fn error(&mut self, _ctx: &RaceContext<S>, errors: Vec<String>) -> Result<(), Error> {
        Err(Error::Server(errors))
    }

    /// Called when a `pong` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn pong(&mut self, _ctx: &RaceContext<S>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn pong(&mut self, _ctx: &RaceContext<S>) -> Result<(), Error> { Ok(()) }

    /// Called when a `race.data` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn race_data(&mut self, _ctx: &RaceContext<S>, _old_race_data: RaceData) -> Result<(), Error>;
    /// ```
    ///
    /// The new race data can be found in the [`RaceContext`] parameter. The [`RaceData`] parameter contains the previous data.
    ///
    /// The default implementation does nothing.
    async fn race_data(&mut self, _ctx: &RaceContext<S>, _old_race_data: RaceData) -> Result<(), Error> { Ok(()) }

    /// Called when a `race.renders` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn race_renders(&mut self, _ctx: &RaceContext<S>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn race_renders(&mut self, _ctx: &RaceContext<S>) -> Result<(), Error> { Ok(()) }

    /// Called when a `race.split` message is received.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn race_split(&mut self, _ctx: &RaceContext<S>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn race_split(&mut self, _ctx: &RaceContext<S>) -> Result<(), Error> { Ok(()) }

    /// Called when a room handler task is created.
    ///
    /// Equivalent to:
    ///
    /// ```ignore
    /// async fn task(_state: Arc<S>, _race_data: Arc<RwLock<RaceData>>, _join_handle: tokio::task::JoinHandle<()>) -> Result<(), Error>;
    /// ```
    ///
    /// The default implementation does nothing.
    async fn task(_state: Arc<S>, _race_data: Arc<RwLock<RaceData>>, _join_handle: tokio::task::JoinHandle<()>) -> Result<(), Error> { Ok(()) }
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

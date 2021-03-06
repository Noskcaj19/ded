use serenity::model::channel;
use serenity::model::event::MessageUpdateEvent;
use serenity::model::id::{ChannelId, MessageId, UserId};
use serenity::prelude::Mutex;
use serenity::prelude::RwLock;
use serenity::utils::Colour;
use termbuf::Color;
use termbuf::Style;
use termbuf::TermSize;
use textwrap::fill;

use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::io;
use std::sync::Arc;

use discord::utils;
use model::{Application, Context, MessageItem};
use view::terminal::Terminal;

const LEFT_PADDING: usize = 20;
const RIGHT_PADDING: usize = 5;
const TIME_PADDING: usize = 3;
const LEFT_START: usize = 5;
const LEFT_START_EXTENDED: usize = 30;
const TOP_START: usize = 1;
const BOTTOM_DIFF: usize = 6;

fn color_to_8bit(colour: ::serenity::utils::Colour) -> Color {
    let r = (u16::from(colour.r()) * 5 / 255) as u8;
    let g = (u16::from(colour.g()) * 5 / 255) as u8;
    let b = (u16::from(colour.b()) * 5 / 255) as u8;
    Color::AnsiValue(16 + 36 * r + 6 * g + b)
}

pub struct Messages {
    pub messages: RefCell<Vec<MessageItem>>,
    max_name_len: RefCell<usize>,
    timestamp_fmt: String,
    truecolor: bool,
    nickname_cache: RefCell<HashMap<UserId, (String, Option<Colour>)>>,
    show_sidebar: Arc<Mutex<bool>>,
}

impl Messages {
    pub fn new(timestamp_fmt: String, show_sidebar: bool) -> Messages {
        let truecolor = match env::var("COLORTERM") {
            Ok(term) => term.to_lowercase() == "truecolor",
            Err(_) => false,
        };

        Messages {
            messages: RefCell::new(Vec::new()),
            max_name_len: RefCell::new(0),
            timestamp_fmt,
            truecolor,
            nickname_cache: RefCell::new(HashMap::new()),
            show_sidebar: Arc::new(Mutex::new(show_sidebar)),
        }
    }

    pub fn set_show_sidebar(&self, state: bool) {
        *self.show_sidebar.lock() = state
    }

    pub fn showing_sidebar(&self) -> bool {
        *self.show_sidebar.lock()
    }

    pub fn add_msg(&self, msg: MessageItem) {
        self.messages.borrow_mut().push(msg);
    }

    pub fn delete_msg(&self, channel_id: ChannelId, message_id: MessageId) {
        let mut msg_index = None;
        for (i, msg) in self.messages.borrow().iter().enumerate() {
            match msg {
                MessageItem::DiscordMessage(msg) => {
                    debug!("Deleting message: {}", message_id);
                    if msg.channel_id == channel_id && msg.id == message_id {
                        msg_index = Some(i);
                        break;
                    }
                }
            }
        }
        if let Some(index) = msg_index {
            self.messages.borrow_mut().remove(index);
        }
    }

    pub fn delete_msg_bulk(&self, channel_id: ChannelId, message_ids: &[MessageId]) {
        debug!(
            "Bulk delete: {}",
            message_ids
                .iter()
                .map(|msg_id| msg_id.0.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        self.messages.borrow_mut().retain(|msg| match msg {
            MessageItem::DiscordMessage(msg) => {
                msg.channel_id != channel_id && !message_ids.contains(&msg.id)
            }
        });
    }

    pub fn update_message(&self, update: MessageUpdateEvent) {
        for mut msg in self.messages.borrow_mut().iter_mut() {
            match msg {
                MessageItem::DiscordMessage(ref mut msg) => {
                    if update.id == msg.id && update.channel_id == msg.channel_id {
                        debug!("Updated message: {}", msg.id);
                        utils::update_msg(msg, update);
                        break;
                    }
                }
            }
        }
    }

    pub fn load_messages(&self, app: &Application) {
        use serenity::builder::GetMessages;

        let num = app.view.terminal_size.height;
        let retriever = GetMessages::default().limit(num as u64);
        if let Some(channel) = app.context.read().channel {
            self.messages.borrow_mut().clear();

            for message in channel
                .messages(|_| retriever)
                .unwrap()
                .iter()
                .rev()
                .cloned()
            {
                self.add_msg(MessageItem::DiscordMessage(Box::new(message)));
            }
        }
    }

    fn put_nick(&self, message: &channel::Message, screen: &mut Terminal, x: usize, y: usize) {
        let mut cache = self.nickname_cache.borrow_mut();
        let entry = cache.entry(message.author.id);

        use std::collections::hash_map::Entry::*;
        let (nick, colour) = match entry {
            Occupied(o) => o.into_mut(),
            Vacant(v) => {
                if let Some(member) = utils::member(&message) {
                    v.insert((
                        member
                            .nick
                            .clone()
                            .unwrap_or_else(|| message.author.name.to_owned()),
                        member.colour(),
                    ))
                } else {
                    v.insert((message.author.name.to_owned(), None))
                }
            }
        };

        if nick.len() > *self.max_name_len.borrow() {
            *self.max_name_len.borrow_mut() = nick.len();
        }
        match colour {
            Some(colour) => {
                if self.truecolor {
                    screen
                        .buf
                        .string_builder(x, y, nick)
                        .fg(Color::Rgb(colour.r(), colour.g(), colour.b()))
                        .draw();
                } else {
                    screen
                        .buf
                        .string_builder(x, y, nick)
                        .fg(color_to_8bit(*colour))
                        .draw();
                }
            }
            None => {
                screen.buf.print(x, y, &nick);
            }
        }
    }

    pub fn render(
        &self,
        screen: &mut Terminal,
        size: TermSize,
        context: &Arc<RwLock<Context>>,
    ) -> Result<(), io::Error> {
        self.set_show_sidebar(context.read().guild_sidebar_visible);

        let rough_msg_count = size.height;
        let mut msgs = self.messages.borrow_mut();
        let msg_diff = msgs.len().saturating_sub(rough_msg_count as usize);

        msgs.drain(0..msg_diff);

        let mut messages = msgs.clone();

        let mut y = size.height.saturating_sub(BOTTOM_DIFF + 1);
        for mut msg in messages.iter_mut().rev() {
            match msg {
                MessageItem::DiscordMessage(msg) => {
                    if !self.render_discord_msg(msg, &mut y, size, screen, context)? {
                        break;
                    };
                }
            }
        }
        Ok(())
    }

    fn render_discord_msg(
        &self,
        msg: &mut channel::Message,
        y: &mut usize,
        size: TermSize,
        screen: &mut Terminal,
        context: &Arc<RwLock<Context>>,
    ) -> Result<bool, io::Error> {
        // Show an indicator if an attachement is present
        let content = if !msg.attachments.is_empty() {
            format!("{} {}", context.read().char_set.paper_clip(), msg.content)
        } else {
            msg.content.to_owned()
        };

        let left_start = if self.showing_sidebar() {
            LEFT_START_EXTENDED
        } else {
            LEFT_START
        };

        let wrapped_lines: Vec<String> = content
            .lines()
            .map(|line| {
                fill(
                    line,
                    (size.width as usize)
                        .saturating_sub(RIGHT_PADDING + LEFT_PADDING + left_start + TIME_PADDING),
                )
            })
            .collect();
        msg.content = wrapped_lines.join("\n");

        let lines: Vec<_> = msg.content.lines().rev().collect();
        for (i, line) in lines.iter().enumerate() {
            if i == (lines.len() - 1) {
                let timestamp = msg
                    .timestamp
                    .with_timezone(&::chrono::offset::Local)
                    .format(&self.timestamp_fmt)
                    .to_string();
                let timestamp_len = timestamp.len();
                let timestamp = timestamp + &if msg.edited_timestamp.is_some() {
                    "*"
                } else {
                    ""
                };
                self.put_nick(&msg, screen, left_start + timestamp_len + 1, *y + TOP_START);
                screen
                    .buf
                    .string_builder(left_start.saturating_sub(2), *y + TOP_START, &timestamp)
                    .style(Style::Faint)
                    .draw();
            }
            screen.buf.print(
                10 + left_start + *self.max_name_len.borrow(),
                *y + TOP_START,
                line,
            );
            if *y == 0 {
                return Ok(false);
            }
            *y -= 1;
        }
        Ok(true)
    }
}

use anyhow::Result;
use die::{die, Die};
use env_logger::{Builder, Env, Target};
use futures::stream::StreamExt;
use log::{info, trace};
use serde_derive::Deserialize;
use std::{convert::TryFrom, fs::File, io::Read, iter::Iterator, path::Path, str::FromStr};
use tokio::io::{self, AsyncBufReadExt, BufReader};
use tokio_xmpp::{AsyncClient as Client, Event};
use xmpp_parsers::{
    message::{Body, Message, MessageType},
    muc::Muc,
    presence::{Presence, Show, Type as PresenceType},
    BareJid, Element, Jid,
};

#[derive(Deserialize)]
struct Config {
    jid: String,
    password: String,
    log_level: Option<String>,
    log_style: Option<String>,
}

fn parse_cfg<P: AsRef<Path>>(path: P) -> Result<Config> {
    let mut f = File::open(path)?;
    let mut input = String::new();
    f.read_to_string(&mut input)?;
    Ok(toml::from_str(&input)?)
}

struct Context {
    bare_me: BareJid,

    contact: Jid,
    bare_contact: BareJid,
    is_muc: bool,
}

impl Context {
    fn new(bare_me: BareJid, contact: Jid) -> Context {
        Self {
            bare_me,
            bare_contact: contact.to_bare(),
            is_muc: matches!(contact, Jid::Full(_)),
            contact,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = Args::default();

    if args.flags(&["-h", "--help"]) {
        die!("usage: kiss-xmpp [-c /path/to/config.toml] user@domain OR room@domain/your-room-nick")
    }

    let cfg = match args.get_option(&["-c", "--config"]) {
        Some(config) => parse_cfg(config).die("provided config cannot be found/parsed"),
        None => parse_cfg(
            dirs::config_dir()
                .die("cannot find home directory")
                .join("kiss-xmpp.toml"),
        )
        .die("valid config file not found"),
    };

    let mut remaining = args.remaining();
    if remaining.len() != 1 {
        eprintln!("error: must have exactly 1 JID to chat with");
        die!("usage: kiss-xmpp [-c /path/to/config.toml] user@domain OR room@domain/your-room-nick")
    }
    let contact = remaining
        .pop()
        .expect("never panics due to length check above");
    let bare_me = BareJid::from_str(&cfg.jid).die("invalid account jid from config file");
    let contact = Jid::from_str(&contact).die("invalid contact jid on command line");

    let env = Env::default()
        .filter_or("KISS_XMPP_LOG_LEVEL", "info")
        .write_style_or("KISS_XMPP_LOG_STYLE", "never");
    let mut builder = Builder::from_env(env);
    builder.target(Target::Stdout);
    if let Some(ref log_level) = cfg.log_level {
        builder.parse_filters(log_level);
    }
    if let Some(ref log_style) = cfg.log_style {
        builder.parse_write_style(log_style);
    }
    // todo: config for this: builder.format_timestamp(None);
    builder.init();

    let context = Context::new(bare_me, contact);

    let stdin = io::stdin();
    let reader = BufReader::new(stdin);
    let mut lines = reader.lines();

    let mut client = Client::new(context.bare_me.clone(), &cfg.password);
    client.set_reconnect(true);

    loop {
        tokio::select! {
        Some(event) = client.next() => handle_xmpp(event, &mut client, &context).await?,
        Ok(Some(line)) = lines.next_line() => {
            if handle_line(line, &mut client, &context).await? {
                break;
            }
        }
        }
    }

    // Close client connection
    client.send_end().await.ok(); // ignore errors here, I guess

    Ok(())
}

async fn handle_xmpp(event: Event, client: &mut Client, context: &Context) -> Result<()> {
    if event.is_online() {
        if context.is_muc {
            let join = make_join(context.contact.clone());
            client.send_stanza(join).await?;
            println!("NOTICE: sent room join!");
        } else {
            let presence = make_presence();
            client.send_stanza(presence).await?;
            let carbons = make_carbons_enable();
            client.send_stanza(carbons).await?;
            println!("NOTICE: online!");
        }
    } else if let Some(element) = event.into_stanza() {
        handle_xmpp_element(element, context).await?;
    }
    Ok(())
}

#[async_recursion::async_recursion]
async fn handle_xmpp_element(element: Element, context: &Context) -> Result<()> {
    if let Ok(message) = Message::try_from(element) {
        trace!("whole message: {message:?}");
        match (message.from, message.bodies.get("")) {
            (Some(from), Some(body)) => {
                let bare_from = from.to_bare();
                if bare_from == context.bare_contact || bare_from == context.bare_me {
                    let from = match from {
                        Jid::Full(jid) => {
                            if context.is_muc {
                                jid.resource_str().to_string()
                            } else {
                                bare_from.to_string()
                            }
                        }
                        from => from.to_string(),
                    };
                    let body = &body.0;
                    let muc_pm = if message
                        .payloads
                        .iter()
                        .any(|e| e.is("x", "http://jabber.org/protocol/muc#user"))
                    {
                        "(PM)"
                    } else {
                        ""
                    };
                    // without this multi-line messages can spoof from, and \r can hide messages
                    for line in body.split(&['\n', '\r']) {
                        println!("{muc_pm}<{from}> {}", line.trim());
                    }
                } else {
                    info!("ignoring: from: '{from}', body: {body:?}");
                }
            }
            (Some(from), None) => {
                // maybe carbons
                if context.bare_me == from {
                    // we can trust this if it's carbons
                    trace!("got a carbon");
                    if let Some(carbon) = message
                        .payloads
                        .into_iter()
                        .find(|e| {
                            e.is("sent", "urn:xmpp:carbons:2")
                                || e.is("received", "urn:xmpp:carbons:2")
                        })
                        .and_then(|mut carbon| {
                            carbon
                                .remove_child("forwarded", "urn:xmpp:forward:0")
                                .and_then(|mut forwarded| {
                                    forwarded.remove_child("message", minidom::NSChoice::Any)
                                })
                        })
                    {
                        // recurse!
                        trace!("found and recursing on carbon");
                        return handle_xmpp_element(carbon, context).await;
                    }
                }
            }
            _ => info!("ignoring message"),
        }
    }
    Ok(())
}

/// true to quit, false otherwise
async fn handle_line(line: String, client: &mut Client, context: &Context) -> Result<bool> {
    let line = line.trim();
    match line {
        "/quit" => return Ok(true),
        "" => return Ok(false),
        _ => {}
    }
    let msg = make_message(
        context.bare_contact.clone().into(),
        context.is_muc,
        line.to_string(),
    );
    client.send_stanza(msg).await?;
    if !context.is_muc {
        // no reflections for 1:1, we will just print on sending
        println!("<{}> {line}", context.bare_me)
    }
    Ok(false)
}

fn make_join(to: Jid) -> Element {
    Presence::new(PresenceType::None)
        .with_to(to)
        .with_payloads(vec![Muc::new()
            // .with_history(History::new().with_maxstanzas(0))
            .into()])
        .into()
}

fn make_presence() -> Element {
    let mut presence = Presence::new(PresenceType::None);
    presence.show = Some(Show::Chat);
    presence.into()
}

fn make_carbons_enable() -> Element {
    r#"<iq xmlns='jabber:client'
    id='enable1'
    type='set'>
      <enable xmlns='urn:xmpp:carbons:2'/>
    </iq>
    "#
    .parse()
    .expect("known valid")
}

// Construct a chat <message/>
fn make_message(to: Jid, is_muc: bool, body: String) -> Element {
    let mut message = Message::new(Some(to));
    if is_muc {
        message.type_ = MessageType::Groupchat;
    }
    message.bodies.insert(String::new(), Body(body));
    message.into()
}

/// boring command line handling stuff down here
pub struct Args {
    args: Vec<String>,
}

impl Args {
    pub fn new(args: Vec<String>) -> Args {
        Args { args }
    }
    pub fn flags(&mut self, flags: &[&str]) -> bool {
        let mut i = 0;
        while i < self.args.len() {
            if flags.contains(&self.args[i].as_str()) {
                self.args.remove(i);
                return true;
            } else {
                i += 1;
            }
        }
        false
    }
    pub fn flag(&mut self, flag: &str) -> bool {
        self.flags(&[flag])
    }
    pub fn get_option(&mut self, flags: &[&str]) -> Option<String> {
        let mut i = 0;
        while i < self.args.len() {
            if flags.contains(&self.args[i].as_str()) {
                // remove the flag
                self.args.remove(i);
                return if i < self.args.len() {
                    Some(self.args.remove(i))
                } else {
                    None
                };
            } else {
                i += 1;
            }
        }
        None
    }
    pub fn get_str(&mut self, flags: &[&str], def: &str) -> String {
        match self.get_option(flags) {
            Some(ret) => ret,
            None => def.to_owned(),
        }
    }
    pub fn remaining(self) -> Vec<String> {
        self.args
    }
}

impl Default for Args {
    fn default() -> Self {
        Self::new(std::env::args().skip(1).collect())
    }
}

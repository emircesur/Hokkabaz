//! Email Trigger (IMAP) (`n8n-nodes-base.emailReadImap`) as a polling trigger.
//!
//! Each poll opens a short IMAP session, searches `UNSEEN` in the mailbox, fetches the
//! newest `maxMessages` with `BODY.PEEK[]` (so fetching does not mark them read), parses
//! them with mail-parser and optionally flags them `\Seen`. Messages are deduped by
//! `<uidvalidity>:<uid>`, so with "leave unread" a message is still emitted once.
//!
//! TLS: `secure` is implicit TLS. Otherwise STARTTLS is attempted and the session falls
//! back to plain text only if the server refuses it (same as the SMTP side). Unseen mail
//! that exists at activation is emitted by the first poll: unseen means not handled yet.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_imap::{Client, Session};
use async_trait::async_trait;
use base64::Engine;
use futures_util::TryStreamExt;
use indexmap::IndexMap;
use mail_parser::{Address, MessageParser, MimeHeaders};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;

use hk_flow::description::NodeTypeDescription;
use hk_flow::item::BinaryData;
use hk_flow::{FlowError, NodeRegistry};

use super::{IMAP_CREDENTIAL, tls_config};
use crate::credentials::CredentialTypes;
use crate::describe::{Choice, NodeBuilder, Prop, poll_times_prop};
use crate::kit::{self, err};
use crate::poll::{PollBatch, PollContext, PollRegistry, PolledItem, PollingTrigger};
use crate::template::display;

pub const NODE_TYPE: &str = "n8n-nodes-base.emailReadImap";

const DEFAULT_MAX_MESSAGES: usize = 100;
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

// ------------------------------------------------------------------- parse

/// One attachment of a parsed mail.
#[derive(Debug, Clone, PartialEq)]
pub struct MailAttachment {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedMail {
    pub json: Value,
    pub attachments: Vec<MailAttachment>,
}

fn address_json(a: Option<&Address<'_>>) -> Value {
    let Some(a) = a else { return Value::Null };
    let flat: Vec<(Option<String>, Option<String>)> = match a {
        Address::List(l) => l
            .iter()
            .map(|x| {
                (
                    x.name.as_ref().map(|s| s.to_string()),
                    x.address.as_ref().map(|s| s.to_string()),
                )
            })
            .collect(),
        Address::Group(g) => g
            .iter()
            .flat_map(|g| g.addresses.iter())
            .map(|x| {
                (
                    x.name.as_ref().map(|s| s.to_string()),
                    x.address.as_ref().map(|s| s.to_string()),
                )
            })
            .collect(),
    };
    let text = flat
        .iter()
        .map(|(n, addr)| match (n, addr) {
            (Some(n), Some(a)) if !n.is_empty() => format!("{n} <{a}>"),
            (_, Some(a)) => a.clone(),
            (Some(n), None) => n.clone(),
            _ => String::new(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    let value: Vec<Value> = flat
        .into_iter()
        .map(|(n, a)| json!({ "address": a, "name": n.unwrap_or_default() }))
        .collect();
    json!({ "value": value, "text": text })
}

/// Parse a raw RFC 5322 message into the item json (n8n "simple" shape) and its attachments.
pub fn parse_message(raw: &[u8], uid: u32) -> Result<ParsedMail, String> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or("Could not parse the email")?;
    let mut m = Map::new();
    m.insert("uid".into(), json!(uid));
    if let Some(id) = msg.message_id() {
        m.insert("messageId".into(), json!(format!("<{id}>")));
    }
    if let Some(s) = msg.subject() {
        m.insert("subject".into(), json!(s));
    }
    if let Some(d) = msg.date() {
        m.insert("date".into(), json!(d.to_rfc3339()));
    }
    for (key, addr) in [
        ("from", msg.from()),
        ("to", msg.to()),
        ("cc", msg.cc()),
        ("bcc", msg.bcc()),
        ("replyTo", msg.reply_to()),
    ] {
        let v = address_json(addr);
        if !v.is_null() {
            m.insert(key.into(), v);
        }
    }
    if let Some(t) = msg.body_text(0) {
        m.insert("text".into(), json!(t));
    }
    if let Some(h) = msg.body_html(0) {
        m.insert("html".into(), json!(h));
    }
    let mut headers = Map::new();
    for (name, value) in msg.headers_raw() {
        headers
            .entry(name.to_ascii_lowercase())
            .or_insert_with(|| json!(value.trim()));
    }
    m.insert("headers".into(), Value::Object(headers));

    let mut attachments = Vec::new();
    for (n, part) in msg.attachments().enumerate() {
        let mime = part
            .content_type()
            .map(|c| match c.subtype() {
                Some(s) => format!("{}/{s}", c.ctype()),
                None => c.ctype().to_owned(),
            })
            .unwrap_or_else(|| "application/octet-stream".into());
        attachments.push(MailAttachment {
            name: part
                .attachment_name()
                .map_or_else(|| format!("attachment_{n}"), str::to_owned),
            mime,
            data: part.contents().to_vec(),
        });
    }
    m.insert(
        "attachments".into(),
        Value::Array(
            attachments
                .iter()
                .map(|a| json!({ "filename": a.name, "contentType": a.mime, "size": a.data.len() }))
                .collect(),
        ),
    );
    Ok(ParsedMail {
        json: Value::Object(m),
        attachments,
    })
}

/// Attachments as item binary properties `<prefix>0`, `<prefix>1`, ...
pub fn attachment_binaries(prefix: &str, atts: &[MailAttachment]) -> IndexMap<String, BinaryData> {
    atts.iter()
        .enumerate()
        .map(|(i, a)| {
            let ext = a.name.rsplit_once('.').map(|(_, e)| e.to_owned());
            (
                format!("{prefix}{i}"),
                BinaryData {
                    data: base64::engine::general_purpose::STANDARD.encode(&a.data),
                    mime_type: a.mime.clone(),
                    file_type: a.mime.split('/').next().map(str::to_owned),
                    file_name: Some(a.name.clone()),
                    file_extension: ext,
                    file_size: Some(json!(a.data.len().to_string())),
                    ..BinaryData::default()
                },
            )
        })
        .collect()
}

// ----------------------------------------------------------------- session

trait Io: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + fmt::Debug> Io for T {}
type Stream = Box<dyn Io>;

async fn upgrade(host: &str, io: Stream) -> Result<Stream, String> {
    let cfg = tls_config().ok_or("TLS is not available")?;
    let name = ServerName::try_from(host.to_owned())
        .map_err(|_| format!("`{host}` is not a valid TLS server name"))?;
    let tls = TlsConnector::from(cfg)
        .connect(name, io)
        .await
        .map_err(|e| format!("TLS handshake failed: {e}"))?;
    Ok(Box::new(tls))
}

async fn open(cred: &Map<String, Value>) -> Result<Session<Stream>, String> {
    let get = |k: &str| cred.get(k).map(display).unwrap_or_default();
    let host = get("host");
    if host.is_empty() {
        return Err("The IMAP credential has no host".into());
    }
    let secure = !matches!(cred.get("secure"), Some(Value::Bool(false)));
    let port: u16 = get("port")
        .trim()
        .parse()
        .unwrap_or(if secure { 993 } else { 143 });
    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| format!("Could not connect to the IMAP server: {e}"))?;

    let mut client: Client<Stream> = if secure {
        Client::new(upgrade(&host, Box::new(tcp)).await?)
    } else {
        Client::new(Box::new(tcp))
    };
    client
        .read_response()
        .await
        .map_err(|e| format!("No IMAP greeting: {e}"))?
        .ok_or("The IMAP server closed the connection")?;
    if !secure
        && client
            .run_command_and_check_ok("STARTTLS", None)
            .await
            .is_ok()
    {
        client = Client::new(upgrade(&host, client.into_inner()).await?);
    }
    client
        .login(get("user"), get("password"))
        .await
        .map_err(|(e, _)| format!("IMAP login failed: {e}"))
}

/// What a poll takes from the mailbox.
#[derive(Debug, Clone)]
struct Fetch {
    mailbox: String,
    mark_read: bool,
    max: usize,
}

async fn fetch_unseen(
    cred: &Map<String, Value>,
    f: &Fetch,
) -> Result<Vec<(u32, u32, Vec<u8>)>, String> {
    let mut s = open(cred).await?;
    let e = |what: &str| {
        let what = what.to_owned();
        move |er: async_imap::error::Error| format!("IMAP {what} failed: {er}")
    };
    let mailbox = s.select(&f.mailbox).await.map_err(e("SELECT"))?;
    let validity = mailbox.uid_validity.unwrap_or(0);
    let mut uids: Vec<u32> = s
        .uid_search("UNSEEN")
        .await
        .map_err(e("SEARCH"))?
        .into_iter()
        .collect();
    // newest first so a backlog cannot starve fresh mail; emitted oldest first
    uids.sort_unstable_by(|a, b| b.cmp(a));
    uids.truncate(f.max);
    uids.sort_unstable();

    let mut out = Vec::new();
    if !uids.is_empty() {
        let set = uids
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let fetched: Vec<_> = s
            .uid_fetch(&set, "(UID BODY.PEEK[])")
            .await
            .map_err(e("FETCH"))?
            .try_collect()
            .await
            .map_err(e("FETCH"))?;
        for m in &fetched {
            if let (Some(uid), Some(body)) = (m.uid, m.body()) {
                out.push((validity, uid, body.to_vec()));
            }
        }
        out.sort_by_key(|(_, uid, _)| *uid);
        if f.mark_read {
            let _: Vec<_> = s
                .uid_store(&set, "+FLAGS (\\Seen)")
                .await
                .map_err(e("STORE"))?
                .try_collect()
                .await
                .map_err(e("STORE"))?;
        }
    }
    // best effort: the data is already in hand
    let _ = s.logout().await;
    Ok(out)
}

// ----------------------------------------------------------------- trigger

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "Email Trigger (IMAP)")
        .description("Starts the workflow when new email arrives in an IMAP mailbox")
        .icon("fa:inbox")
        .color("#44aa77")
        .polling_trigger()
        .credential(IMAP_CREDENTIAL, true)
        .property(Prop::string("mailbox", "Mailbox Name").default("INBOX"))
        .property(Prop::options(
            "postProcessAction",
            "Action",
            vec![
                Choice::new("Mark as Read", "read"),
                Choice::new("Nothing", "nothing"),
            ],
        ))
        .property(Prop::options(
            "format",
            "Format",
            vec![
                Choice::new("Simple", "simple")
                    .describe("Parsed fields: headers, text, html, addresses"),
                Choice::new("Resolved", "resolved")
                    .describe("Simple, plus attachments as binary data"),
                Choice::new("RAW", "raw")
                    .describe("Simple, plus the whole message base64 encoded in `raw`"),
            ],
        ))
        .property(Prop::collection(
            "options",
            "Options",
            vec![
                Prop::boolean("downloadAttachments", "Download Attachments").description(
                    "Whether to add attachments as binary data (always on for format Resolved)",
                ),
                Prop::string("dataPropertyAttachmentsPrefixName", "Attachment Prefix")
                    .default("attachment_")
                    .description("Binary properties are named prefix + index"),
                Prop::number("maxMessages", "Max Messages Per Poll")
                    .default(DEFAULT_MAX_MESSAGES as u64)
                    .min(1.0),
            ],
        ))
        .property(poll_times_prop())
        .build();
    kit::serve_versions(&mut d, &[1.0, 2.0, 2.1]);
    d
}

#[derive(Debug)]
struct ImapTrigger {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl PollingTrigger for ImapTrigger {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    fn emit_on_first_poll(&self) -> bool {
        true
    }

    async fn poll(
        &self,
        cx: &PollContext,
        _cursor: Option<&Value>,
    ) -> Result<PollBatch, FlowError> {
        let node = cx.node().name.clone();
        let raw = cx.credential(IMAP_CREDENTIAL).await?;
        let cred = self.creds.with_defaults(IMAP_CREDENTIAL, &raw);

        let mailbox = cx.param_str("mailbox", "INBOX")?;
        let format = cx.param_str("format", "simple")?;
        let max = cx
            .param("options.maxMessages")?
            .and_then(|v| v.as_u64().or_else(|| display(&v).parse().ok()))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_MESSAGES);
        let want_files = format == "resolved"
            || matches!(
                cx.param("options.downloadAttachments")?,
                Some(Value::Bool(true))
            );
        let prefix = cx.param_str("options.dataPropertyAttachmentsPrefixName", "attachment_")?;
        let f = Fetch {
            mailbox,
            mark_read: cx.param_str("postProcessAction", "read")? == "read",
            max,
        };

        let mails = tokio::time::timeout(SESSION_TIMEOUT, fetch_unseen(&cred, &f))
            .await
            .map_err(|_| err(&node, "The IMAP session timed out"))?
            .map_err(|m| err(&node, m))?;

        let mut items = Vec::with_capacity(mails.len());
        for (validity, uid, bytes) in mails {
            let parsed = match parse_message(&bytes, uid) {
                Ok(p) => p,
                Err(m) => {
                    // one malformed mail must not block the rest (it is marked read already)
                    tracing::warn!(node = %node, uid, error = %m, "skipping unparsable email");
                    continue;
                }
            };
            let mut json = parsed.json;
            if format == "raw"
                && let Value::Object(o) = &mut json
            {
                o.insert(
                    "raw".into(),
                    json!(base64::engine::general_purpose::STANDARD.encode(&bytes)),
                );
            }
            let mut item = PolledItem::new(format!("{validity}:{uid}"), json);
            if want_files {
                item = item.with_binary(attachment_binaries(&prefix, &parsed.attachments));
            }
            items.push(item);
        }
        Ok(PollBatch {
            items,
            cursor: None,
        })
    }
}

pub fn register(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    polls: &mut PollRegistry,
) -> Result<(), FlowError> {
    polls.register(
        nodes,
        Arc::new(ImapTrigger {
            desc: description(),
            creds: creds.clone(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIL: &str = "From: \"Ann Example\" <ann@example.org>\r\n\
To: bob@example.com, Carl <carl@example.com>\r\n\
Cc: dee@example.com\r\n\
Subject: =?UTF-8?Q?Caf=C3=A9_menu?=\r\n\
Date: Tue, 1 Jul 2025 10:52:37 +0200\r\n\
Message-ID: <abc123@example.org>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"B1\"\r\n\
\r\n\
--B1\r\n\
Content-Type: multipart/alternative; boundary=\"B2\"\r\n\
\r\n\
--B2\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Plain body\r\n\
--B2\r\n\
Content-Type: text/html; charset=utf-8\r\n\
\r\n\
<p>HTML body</p>\r\n\
--B2--\r\n\
--B1\r\n\
Content-Type: application/pdf; name=\"menu.pdf\"\r\n\
Content-Disposition: attachment; filename=\"menu.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
JVBERi0xLjQ=\r\n\
--B1--\r\n";

    #[test]
    fn parses_headers_bodies_and_attachment() {
        let p = parse_message(MAIL.as_bytes(), 42).unwrap();
        let j = &p.json;
        assert_eq!(j["uid"], 42);
        assert_eq!(j["subject"], "Café menu");
        assert_eq!(j["messageId"], "<abc123@example.org>");
        assert_eq!(j["from"]["value"][0]["address"], "ann@example.org");
        assert_eq!(j["from"]["value"][0]["name"], "Ann Example");
        assert_eq!(j["to"]["value"].as_array().unwrap().len(), 2);
        assert_eq!(j["to"]["text"], "bob@example.com, Carl <carl@example.com>");
        assert_eq!(j["cc"]["value"][0]["address"], "dee@example.com");
        assert!(
            j["date"]
                .as_str()
                .unwrap()
                .starts_with("2025-07-01T10:52:37")
        );
        assert_eq!(j["text"].as_str().unwrap().trim(), "Plain body");
        assert_eq!(j["html"].as_str().unwrap().trim(), "<p>HTML body</p>");
        assert!(j["headers"]["subject"].as_str().unwrap().contains("Caf"));
        assert_eq!(
            j["attachments"],
            json!([{"filename": "menu.pdf", "contentType": "application/pdf", "size": 8}])
        );
        assert_eq!(p.attachments.len(), 1);
        assert_eq!(p.attachments[0].data, b"%PDF-1.4");
    }

    #[test]
    fn attachments_become_prefixed_binaries() {
        let p = parse_message(MAIL.as_bytes(), 1).unwrap();
        let bin = attachment_binaries("attachment_", &p.attachments);
        let b = &bin["attachment_0"];
        assert_eq!(b.file_name.as_deref(), Some("menu.pdf"));
        assert_eq!(b.file_extension.as_deref(), Some("pdf"));
        assert_eq!(b.mime_type, "application/pdf");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(&b.data)
                .unwrap(),
            b"%PDF-1.4"
        );
        assert!(attachment_binaries("a", &[]).is_empty());
    }

    #[test]
    fn simple_mail_has_no_attachments() {
        let p = parse_message(b"From: a@b.c\r\nSubject: hi\r\n\r\nbody\r\n", 7).unwrap();
        assert_eq!(p.json["attachments"], json!([]));
        assert_eq!(p.json["text"].as_str().unwrap().trim(), "body");
        assert!(p.json.get("cc").is_none());
    }
}

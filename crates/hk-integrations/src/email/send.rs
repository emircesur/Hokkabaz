//! Send Email (`n8n-nodes-base.emailSend`): one message per input item over SMTP (lettre).
//!
//! `secure` on means implicit TLS (465); off means STARTTLS when the server offers it, or
//! plain text with `disableStartTls`. The password goes to lettre's `Credentials` and
//! nowhere else; lettre errors never include it.

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use lettre::message::header::ContentType;
use lettre::message::{Attachment, Mailboxes, MessageBuilder, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use super::SMTP_CREDENTIAL;
use crate::credentials::CredentialTypes;
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit::{self, BinFile, node_err};
use crate::template::display;

pub const NODE_TYPE: &str = "n8n-nodes-base.emailSend";

/// What to send, after parameter evaluation.
#[derive(Debug, Clone, Default)]
pub struct Outgoing {
    pub from: String,
    /// Comma-separated; `Name <a@b.c>` allowed.
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub reply_to: String,
    pub subject: String,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<BinFile>,
}

fn mailboxes(label: &str, list: &str) -> Result<Mailboxes, String> {
    Mailboxes::from_str(list).map_err(|e| format!("Invalid {label} address `{list}`: {e}"))
}

fn message_id(from: &str) -> String {
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let domain = from
        .rsplit_once('@')
        .map(|(_, d)| d.trim_end_matches('>').trim())
        .filter(|d| !d.is_empty())
        .unwrap_or("hokkabaz.local");
    format!("<{nanos:x}.{}@{domain}>", N.fetch_add(1, Ordering::Relaxed))
}

/// Build the MIME message. `Bcc` recipients go in the envelope, not in the headers.
pub fn build_message(m: &Outgoing) -> Result<Message, String> {
    let mut b: MessageBuilder = Message::builder();
    for mb in mailboxes("From", &m.from)? {
        b = b.from(mb);
    }
    for (label, list, kind) in [
        ("To", &m.to, 0),
        ("Cc", &m.cc, 1),
        ("Bcc", &m.bcc, 2),
        ("Reply-To", &m.reply_to, 3),
    ] {
        if list.trim().is_empty() {
            continue;
        }
        for mb in mailboxes(label, list)? {
            b = match kind {
                0 => b.to(mb),
                1 => b.cc(mb),
                2 => b.bcc(mb),
                _ => b.reply_to(mb),
            };
        }
    }
    b = b
        .subject(m.subject.clone())
        .message_id(Some(message_id(&m.from)));

    let result = match (&m.text, &m.html, m.attachments.is_empty()) {
        // a lone body needs no multipart wrapper
        (t, None, true) => b.singlepart(SinglePart::plain(t.clone().unwrap_or_default())),
        (None, Some(h), true) => b.singlepart(SinglePart::html(h.clone())),
        (t, h, _) => {
            let body = match (t, h) {
                (Some(t), Some(h)) => MultiPart::alternative()
                    .singlepart(SinglePart::plain(t.clone()))
                    .singlepart(SinglePart::html(h.clone())),
                (None, Some(h)) => MultiPart::mixed().singlepart(SinglePart::html(h.clone())),
                (t, None) => {
                    MultiPart::mixed().singlepart(SinglePart::plain(t.clone().unwrap_or_default()))
                }
            };
            let mut mixed = MultiPart::mixed().multipart(body);
            for a in &m.attachments {
                let ct = ContentType::parse(&a.mime)
                    .or_else(|_| ContentType::parse("application/octet-stream"))
                    .map_err(|e| format!("Invalid attachment type: {e}"))?;
                mixed = mixed.singlepart(Attachment::new(a.name.clone()).body(a.data.clone(), ct));
            }
            b.multipart(mixed)
        }
    };
    result.map_err(|e| format!("Could not build the email: {e}"))
}

/// SMTP transport for credential `cred`. No connection is made here.
pub fn transport(cred: &Map<String, Value>) -> Result<AsyncSmtpTransport<Tokio1Executor>, String> {
    let get = |k: &str| cred.get(k).map(display).unwrap_or_default();
    let host = get("host");
    if host.is_empty() {
        return Err("The SMTP credential has no host".into());
    }
    let secure = !matches!(cred.get("secure"), Some(Value::Bool(false)));
    let port: u16 = get("port")
        .trim()
        .parse()
        .unwrap_or(if secure { 465 } else { 587 });
    let tls_err = |e: lettre::transport::smtp::Error| format!("Could not set up TLS: {e}");

    let mut b = if secure {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&host).map_err(tls_err)?
    } else if matches!(cred.get("disableStartTls"), Some(Value::Bool(true))) {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
    } else {
        let params = TlsParameters::new(host.clone()).map_err(tls_err)?;
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
            .tls(Tls::Opportunistic(params))
    };
    b = b.port(port).timeout(Some(Duration::from_secs(60)));
    let user = get("user");
    if !user.is_empty() {
        b = b.credentials(Credentials::new(user, get("password")));
    }
    Ok(b.build())
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "Send Email")
        .description("Sends an email using SMTP protocol")
        .subtitle("={{ $parameter.subject }}")
        .icon("fa:envelope")
        .color("#00bb88")
        .credential(SMTP_CREDENTIAL, true)
        .property(
            Prop::string("fromEmail", "From Email")
                .required()
                .placeholder("admin@example.com"),
        )
        .property(
            Prop::string("toEmail", "To Email")
                .required()
                .placeholder("info@example.com"),
        )
        .property(Prop::string("ccEmail", "CC Email"))
        .property(Prop::string("bccEmail", "BCC Email"))
        .property(Prop::string("subject", "Subject"))
        .property(Prop::options(
            "emailFormat",
            "Email Format",
            vec![
                Choice::new("HTML", "html"),
                Choice::new("Text", "text"),
                Choice::new("Both", "both"),
            ],
        ))
        .property(
            Prop::string("text", "Text")
                .type_option("rows", 5)
                .show("emailFormat", &["text", "both"]),
        )
        .property(
            Prop::string("html", "HTML")
                .type_option("rows", 5)
                .show("emailFormat", &["html", "both"]),
        )
        .property(Prop::collection(
            "options",
            "Options",
            vec![
                Prop::string("attachments", "Attachments")
                    .placeholder("data,data2")
                    .description("Comma-separated names of the input binary properties to attach"),
                Prop::string("replyTo", "Reply To Email"),
            ],
        ))
        .build();
    kit::serve_versions(&mut d, &[1.0, 2.0, 2.1]);
    d
}

#[derive(Debug)]
struct EmailSend {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

/// Binary property names to attach: `options.attachments` or v1 `attachments` as a comma
/// list, or v2's `attachments.values[].property`.
fn attachment_names(ctx: &NodeContext, i: usize) -> Result<Vec<String>, FlowError> {
    let mut names = Vec::new();
    for path in ["options.attachments", "attachments"] {
        match kit::field(ctx, path, i)? {
            Some(Value::String(s)) => names.extend(s.split(',').map(|x| x.trim().to_owned())),
            Some(Value::Object(o)) => {
                if let Some(Value::Array(vals)) = o.get("values") {
                    names.extend(vals.iter().filter_map(|v| v.get("property").map(display)));
                }
            }
            _ => {}
        }
    }
    names.retain(|n| !n.is_empty());
    names.dedup();
    Ok(names)
}

impl EmailSend {
    fn outgoing(&self, ctx: &NodeContext, i: usize) -> Result<Outgoing, FlowError> {
        let s = |name: &str| kit::str_param(ctx, &self.desc, name, i);
        let (text, html) = (s("text")?, s("html")?);
        // v1 had no format switch: send whichever bodies are filled in
        let (text, html) = match kit::field_str(ctx, "emailFormat", i)?.as_deref() {
            Some("text") => (Some(text), None),
            Some("html") => (None, Some(html)),
            Some("both") => (Some(text), Some(html)),
            _ => (
                Some(text).filter(|t| !t.is_empty()),
                Some(html).filter(|h| !h.is_empty()),
            ),
        };
        let item = ctx
            .input
            .first()
            .and_then(|v| v.get(i))
            .cloned()
            .unwrap_or_default();
        let attachments = attachment_names(ctx, i)?
            .iter()
            .map(|n| kit::binary_file(ctx, &item, n))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Outgoing {
            from: s("fromEmail")?,
            to: s("toEmail")?,
            cc: s("ccEmail")?,
            bcc: s("bccEmail")?,
            reply_to: kit::field_str(ctx, "options.replyTo", i)?.unwrap_or_default(),
            subject: s("subject")?,
            text,
            html,
            attachments,
        })
    }
}

#[async_trait]
impl FlowNode for EmailSend {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let cred = kit::credential(ctx, &self.creds, SMTP_CREDENTIAL).await?;
        let mailer = transport(&cred).map_err(|m| node_err(ctx, m))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let res = async {
                let msg = build_message(&self.outgoing(ctx, i)?).map_err(|m| node_err(ctx, m))?;
                let id = msg
                    .headers()
                    .get_raw("Message-ID")
                    .map(str::to_owned)
                    .unwrap_or_default();
                let env = msg.envelope().clone();
                let resp = mailer
                    .send(msg)
                    .await
                    .map_err(|e| node_err(ctx, format!("Sending the email failed: {e}")))?;
                let accepted: Vec<String> = env.to().iter().map(ToString::to_string).collect();
                let response = format!(
                    "{} {}",
                    resp.code(),
                    resp.message().collect::<Vec<_>>().join(" ")
                );
                Ok(vec![Item::from_json(json!({
                    "messageId": id,
                    "accepted": accepted,
                    "rejected": [],
                    "envelope": {
                        "from": env.from().map(ToString::to_string),
                        "to": accepted,
                    },
                    "response": response,
                }))])
            }
            .await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    nodes.register(Arc::new(EmailSend {
        desc: description(),
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Outgoing {
        Outgoing {
            from: "Robot <robot@example.com>".into(),
            to: "a@example.org, Bob <bob@example.org>".into(),
            subject: "Héllo".into(),
            ..Outgoing::default()
        }
    }

    fn text(m: &Message) -> String {
        String::from_utf8_lossy(&m.formatted()).into_owned()
    }

    #[test]
    fn plain_message_headers_and_envelope() {
        let m = build_message(&Outgoing {
            text: Some("hi".into()),
            cc: "c@example.org".into(),
            ..base()
        })
        .unwrap();
        let t = text(&m);
        assert!(t.contains("From: Robot <robot@example.com>"), "{t}");
        assert!(
            t.contains("To: a@example.org, Bob <bob@example.org>"),
            "{t}"
        );
        assert!(t.contains("Cc: c@example.org"));
        assert!(
            t.contains("Subject: =?utf-8?"),
            "non-ascii subject is encoded: {t}"
        );
        assert!(t.contains("Message-ID: <") && t.contains("@example.com>"));
        assert!(t.contains("text/plain") && !t.contains("multipart"));
        let rcpt: Vec<String> = m.envelope().to().iter().map(ToString::to_string).collect();
        assert_eq!(rcpt, ["a@example.org", "bob@example.org", "c@example.org"]);
    }

    #[test]
    fn bcc_is_enveloped_not_headed() {
        let m = build_message(&Outgoing {
            text: Some("x".into()),
            bcc: "hidden@example.net".into(),
            ..base()
        })
        .unwrap();
        assert!(!text(&m).contains("hidden@example.net"));
        assert!(
            m.envelope()
                .to()
                .iter()
                .any(|a| a.to_string() == "hidden@example.net")
        );
    }

    #[test]
    fn text_and_html_are_alternatives_with_attachment_in_mixed() {
        let m = build_message(&Outgoing {
            text: Some("plain body".into()),
            html: Some("<b>rich</b>".into()),
            attachments: vec![BinFile {
                data: b"%PDF-1.4".to_vec(),
                mime: "application/pdf".into(),
                name: "r.pdf".into(),
            }],
            reply_to: "help@example.com".into(),
            ..base()
        })
        .unwrap();
        let t = text(&m);
        let mixed = t.find("multipart/mixed").expect("mixed");
        let alt = t.find("multipart/alternative").expect("alternative");
        assert!(mixed < alt, "alternative nests inside mixed");
        assert!(t.contains("text/plain") && t.contains("text/html"));
        assert!(
            t.contains("Content-Disposition: attachment; filename=\"r.pdf\""),
            "{t}"
        );
        assert!(t.contains("application/pdf"));
        assert!(t.contains("Reply-To: help@example.com"));
    }

    #[test]
    fn html_only_has_no_alternative() {
        let m = build_message(&Outgoing {
            html: Some("<p>x</p>".into()),
            ..base()
        })
        .unwrap();
        let t = text(&m);
        assert!(t.contains("text/html") && !t.contains("multipart"));
    }

    #[test]
    fn bad_address_is_reported() {
        let e = build_message(&Outgoing {
            to: "not an address".into(),
            ..base()
        })
        .unwrap_err();
        assert!(e.contains("Invalid To address"), "{e}");
        assert!(
            build_message(&Outgoing {
                from: String::new(),
                ..base()
            })
            .is_err()
        );
    }

    #[test]
    fn transport_modes_build_without_connecting() {
        let c = |v: Value| match v {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        assert!(transport(&c(json!({"host": "smtp.example.com", "port": 465, "secure": true, "user": "u", "password": "p"}))).is_ok());
        assert!(
            transport(&c(
                json!({"host": "smtp.example.com", "port": "587", "secure": false})
            ))
            .is_ok()
        );
        assert!(
            transport(&c(
                json!({"host": "127.0.0.1", "port": 25, "secure": false, "disableStartTls": true})
            ))
            .is_ok()
        );
        assert!(transport(&c(json!({"port": 25}))).is_err());
    }
}

//! Email: Send Email over SMTP (`n8n-nodes-base.emailSend`) and the IMAP Email Trigger
//! (`n8n-nodes-base.emailReadImap`).
//!
//! Pure Rust and android-safe: lettre (tokio + rustls/ring), async-imap over tokio-rustls
//! with the bundled webpki roots, mail-parser. No openssl / native-tls anywhere in the tree,
//! and no TLS verification switch: a server with a private CA is not supported.
//!
//! The wire code (SMTP send, IMAP session) is thin; message building and parsing are plain
//! functions ([`send::build_message`], [`imap::parse_message`]) so they are unit-tested
//! without a server.

pub mod imap;
pub mod send;

use std::sync::{Arc, OnceLock};

use tokio_rustls::rustls::{ClientConfig, RootCertStore};

use hk_flow::{FlowError, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::Prop;
use crate::poll::PollRegistry;

pub const SMTP_CREDENTIAL: &str = "smtp";
pub const IMAP_CREDENTIAL: &str = "imap";

pub fn smtp_credential_type() -> CredentialType {
    CredentialType::new(SMTP_CREDENTIAL, "SMTP")
        .documentation("https://datatracker.ietf.org/doc/html/rfc5321")
        .property(Prop::string("host", "Host").required())
        .property(Prop::number("port", "Port").default(465))
        .property(Prop::string("user", "User"))
        .property(Prop::string("password", "Password").password())
        .property(
            Prop::boolean("secure", "SSL/TLS")
                .default(true)
                .description("Implicit TLS (usually port 465). Off: STARTTLS when the server offers it"),
        )
        .property(
            Prop::boolean("disableStartTls", "Disable STARTTLS")
                .description("Only with SSL/TLS off: never upgrade the connection (plain text, trusted networks only)")
                .show_bool("secure", false),
        )
        .property(Prop::string("hostName", "Client Host Name").description("Name announced in EHLO"))
}

pub fn imap_credential_type() -> CredentialType {
    CredentialType::new(IMAP_CREDENTIAL, "IMAP")
        .documentation("https://datatracker.ietf.org/doc/html/rfc3501")
        .property(Prop::string("host", "Host").required())
        .property(Prop::number("port", "Port").default(993))
        .property(Prop::string("user", "User").required())
        .property(Prop::string("password", "Password").password().required())
        .property(
            Prop::boolean("secure", "SSL/TLS")
                .default(true)
                .description(
                    "Implicit TLS (usually port 993). Off: STARTTLS when the server offers it",
                ),
        )
}

/// rustls client config: ring provider, webpki roots. Built once; `None` when the
/// provider rejects the default protocol versions (cannot happen with ring).
pub(crate) fn tls_config() -> Option<Arc<ClientConfig>> {
    static CONFIG: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
            let cfg = ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .ok()?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Some(Arc::new(cfg))
        })
        .clone()
}

/// Nodes and credential types into the shared registries. Features: `int-email`.
pub fn register(
    nodes: &mut NodeRegistry,
    creds: &CredentialTypes,
    polls: &mut PollRegistry,
) -> Result<(), FlowError> {
    creds.register_if_absent(smtp_credential_type());
    creds.register_if_absent(imap_credential_type());
    send::register(nodes, creds)?;
    imap::register(nodes, creds, polls)
}

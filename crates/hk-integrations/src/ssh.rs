//! SSH (`n8n-nodes-base.ssh`): run a command on a remote host. Password or private-key login.
//!
//! Host key: set `hostKeyFingerprint` (`SHA256:...`, from `ssh-keygen -lf`) in the credential to
//! pin the server. Left empty, any key is accepted (the first connection is trusted); pin it for
//! anything that matters. File transfer (SFTP) is not part of this node.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate, decode_secret_key};
use russh::{ChannelMsg, client};
use serde_json::{Map, Value, json};

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.ssh";
pub const CREDENTIAL: &str = "sshPassword";

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "SSH")
        .property(Prop::string("host", "Host").required())
        .property(Prop::number("port", "Port").default(22))
        .property(Prop::string("username", "Username").required())
        .property(Prop::string("password", "Password").password())
        .property(
            Prop::string("privateKey", "Private Key")
                .password()
                .type_option("rows", 4)
                .description("OpenSSH/PEM private key; used instead of the password when set"),
        )
        .property(Prop::string("passphrase", "Passphrase").password())
        .property(
            Prop::string("hostKeyFingerprint", "Host Key Fingerprint")
                .description("SHA256:... of the server key. Empty: accept any key"),
        )
}

fn description() -> NodeTypeDescription {
    let mut d = NodeBuilder::new(NODE_TYPE, "SSH")
        .description("Execute commands over SSH")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:terminal")
        .color("#000000")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![Choice::new("Execute Command", "execute").action("Execute a command")],
        ))
        .property(Prop::string("command", "Command").required())
        .property(Prop::string("cwd", "Working Directory").description("`cd` here first"))
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

struct Pin(Option<String>);

impl client::Handler for Pin {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let Some(want) = &self.0 else {
            return Ok(true);
        };
        match key {
            PublicKeyOrCertificate::PublicKey { key, .. } => {
                Ok(&key.fingerprint(HashAlg::Sha256).to_string() == want)
            }
            PublicKeyOrCertificate::Certificate(_) => Ok(false),
        }
    }
}

struct Outcome {
    stdout: String,
    stderr: String,
    code: Option<u32>,
}

async fn run(cred: &Map<String, Value>, command: &str) -> Result<Outcome, String> {
    let host = kit::cred_str(cred, "host");
    let port = kit::cred_str(cred, "port").parse::<u16>().unwrap_or(22);
    let user = kit::cred_str(cred, "username");
    let pin = Some(kit::cred_str(cred, "hostKeyFingerprint")).filter(|f| !f.is_empty());
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(Duration::from_secs(120)),
        ..client::Config::default()
    });
    let mut session = tokio::time::timeout(
        Duration::from_secs(20),
        client::connect(config, (host.as_str(), port), Pin(pin)),
    )
    .await
    .map_err(|_| "connection timed out".to_owned())?
    .map_err(|e| format!("could not connect: {e}"))?;

    let key = kit::cred_str(cred, "privateKey");
    let auth = if key.trim().is_empty() {
        session
            .authenticate_password(user, kit::cred_str(cred, "password"))
            .await
    } else {
        let pass = Some(kit::cred_str(cred, "passphrase")).filter(|p| !p.is_empty());
        let pk =
            decode_secret_key(&key, pass.as_deref()).map_err(|e| format!("private key: {e}"))?;
        let hash = session
            .best_supported_rsa_hash()
            .await
            .map_err(|e| e.to_string())?
            .flatten();
        session
            .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(pk), hash))
            .await
    }
    .map_err(|e| format!("login failed: {e}"))?;
    if !auth.success() {
        return Err("login refused (wrong user, password or key)".into());
    }

    let mut ch = session
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    ch.exec(true, command).await.map_err(|e| e.to_string())?;
    let (mut stdout, mut stderr, mut code) = (Vec::new(), Vec::new(), None);
    while let Some(msg) = ch.wait().await {
        match msg {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, .. } => stderr.extend_from_slice(&data),
            ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status),
            _ => {}
        }
    }
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(Outcome {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        code,
    })
}

/// `cd 'dir' && cmd`, with the directory single-quoted.
fn with_cwd(cwd: &str, command: &str) -> String {
    if cwd.trim().is_empty() {
        command.to_owned()
    } else {
        format!("cd '{}' && {command}", cwd.replace('\'', "'\\''"))
    }
}

#[derive(Debug)]
struct SshNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl FlowNode for SshNode {
    fn description(&self) -> &NodeTypeDescription {
        &self.desc
    }

    async fn execute(&self, ctx: &NodeContext) -> Result<NodeOutput, FlowError> {
        let input_len = ctx.input.first().map_or(0, Vec::len);
        if input_len == 0 {
            return Ok(vec![Vec::new()]);
        }
        let name = &ctx.node.name;
        let cred = kit::credential(ctx, &self.creds, CREDENTIAL).await?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let cmd = with_cwd(
                &kit::str_param(ctx, &self.desc, "cwd", i)?,
                &kit::str_param(ctx, &self.desc, "command", i)?,
            );
            let res = run(&cred, &cmd).await.map_err(|m| kit::err(name, m)).map(|o| {
                vec![kit::to_item(json!({"code": o.code, "signal": Value::Null, "stdout": o.stdout, "stderr": o.stderr}))]
            });
            kit::settle(ctx, i, res, &mut out)?;
        }
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-ssh`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(SshNode {
        desc: description(),
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwd_is_quoted() {
        assert_eq!(with_cwd("", "ls"), "ls");
        assert_eq!(with_cwd("/tmp/a b", "ls"), "cd '/tmp/a b' && ls");
        assert_eq!(with_cwd("it's", "ls"), "cd 'it'\\''s' && ls");
    }
}

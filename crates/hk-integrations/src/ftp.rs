//! FTP (`n8n-nodes-base.ftp`): list, download, upload, delete, rename, create folders.
//!
//! A small client over tokio TCP (passive mode, binary type). Plain FTP only: there is no TLS,
//! so use it on trusted networks or prefer SFTP/HTTPS services. Paths and names with CR/LF are
//! refused (they would inject commands).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use hk_flow::description::NodeTypeDescription;
use hk_flow::{FlowError, FlowNode, Item, NodeContext, NodeOutput, NodeRegistry};

use crate::credentials::{CredentialType, CredentialTypes};
use crate::describe::{Choice, NodeBuilder, Prop};
use crate::kit;

pub const NODE_TYPE: &str = "n8n-nodes-base.ftp";
pub const CREDENTIAL: &str = "ftp";
const IO_TIMEOUT: Duration = Duration::from_secs(30);

pub fn credential_type() -> CredentialType {
    CredentialType::new(CREDENTIAL, "FTP")
        .property(Prop::string("host", "Host").required())
        .property(Prop::number("port", "Port").default(21))
        .property(Prop::string("username", "Username").default("anonymous"))
        .property(Prop::string("password", "Password").password())
}

fn description() -> NodeTypeDescription {
    let ops = &["list", "download", "upload", "delete", "rename", "mkdir"];
    let mut d = NodeBuilder::new(NODE_TYPE, "FTP")
        .description("Transfer files over FTP")
        .subtitle("={{ $parameter.operation }}")
        .icon("fa:server")
        .color("#303030")
        .credential(CREDENTIAL, true)
        .property(Prop::options(
            "operation",
            "Operation",
            vec![
                Choice::new("List", "list").action("List a folder"),
                Choice::new("Download", "download").action("Download a file"),
                Choice::new("Upload", "upload").action("Upload a file"),
                Choice::new("Delete", "delete").action("Delete a file"),
                Choice::new("Rename", "rename").action("Rename or move"),
                Choice::new("Create Folder", "mkdir").action("Create a folder"),
            ],
        ))
        .property(
            Prop::string("path", "Path")
                .default("/")
                .required()
                .show("operation", ops),
        )
        .property(
            Prop::string("newPath", "New Path")
                .required()
                .show("operation", &["rename"]),
        )
        .property(
            Prop::string("binaryPropertyName", "Binary Property")
                .default("data")
                .show("operation", &["download", "upload"]),
        )
        .build();
    kit::serve_versions(&mut d, &[1.0]);
    d
}

fn safe(s: &str) -> Result<&str, String> {
    if s.contains(['\r', '\n']) {
        Err("paths must not contain line breaks".into())
    } else {
        Ok(s)
    }
}

struct Ftp {
    ctrl: BufReader<TcpStream>,
    host: String,
}

impl Ftp {
    async fn reply(&mut self) -> Result<(u16, String), String> {
        let mut text = String::new();
        loop {
            let mut line = String::new();
            let n = tokio::time::timeout(IO_TIMEOUT, self.ctrl.read_line(&mut line))
                .await
                .map_err(|_| "server did not answer".to_owned())?
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("connection closed by the server".into());
            }
            text.push_str(&line);
            let b = line.as_bytes();
            // "123 text" ends a reply; "123-text" continues it
            if b.len() >= 4 && b[..3].iter().all(u8::is_ascii_digit) && b[3] == b' ' {
                let code = line[..3].parse().unwrap_or(0);
                return Ok((code, text.trim().to_owned()));
            }
        }
    }

    async fn cmd(&mut self, c: &str) -> Result<(u16, String), String> {
        safe(c)?;
        self.ctrl
            .get_mut()
            .write_all(format!("{c}\r\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.reply().await
    }

    async fn expect(&mut self, c: &str, ok: &[u16]) -> Result<String, String> {
        let (code, text) = self.cmd(c).await?;
        if ok.contains(&code) {
            Ok(text)
        } else {
            // never echo PASS
            let shown = if c.starts_with("PASS ") { "PASS" } else { c };
            Err(format!("{shown}: {text}"))
        }
    }

    async fn connect(cred: &Map<String, Value>) -> Result<Self, String> {
        let host = kit::cred_str(cred, "host");
        let port = kit::cred_str(cred, "port").parse::<u16>().unwrap_or(21);
        let s = tokio::time::timeout(IO_TIMEOUT, TcpStream::connect((host.as_str(), port)))
            .await
            .map_err(|_| "connection timed out".to_owned())?
            .map_err(|e| format!("could not connect: {e}"))?;
        let mut f = Self {
            ctrl: BufReader::new(s),
            host,
        };
        let (code, text) = f.reply().await?;
        if code != 220 {
            return Err(format!("unexpected greeting: {text}"));
        }
        let user = Some(kit::cred_str(cred, "username"))
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "anonymous".into());
        let (code, text) = f.cmd(&format!("USER {user}")).await?;
        match code {
            230 => {}
            331 | 332 => {
                let pass = kit::cred_str(cred, "password");
                f.expect(&format!("PASS {pass}"), &[230, 202])
                    .await
                    .map_err(|_| "login refused".to_owned())?;
            }
            _ => return Err(format!("login refused: {text}")),
        }
        f.expect("TYPE I", &[200]).await?;
        Ok(f)
    }

    /// Open a passive data connection.
    async fn data(&mut self) -> Result<TcpStream, String> {
        let text = self.expect("PASV", &[227]).await?;
        let nums: Vec<u16> = text
            .split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        // "227 Entering Passive Mode (h1,h2,h3,h4,p1,p2)": the last six numbers
        let n = nums.len();
        if n < 6 {
            return Err(format!("cannot read the passive address: {text}"));
        }
        let port = nums[n - 2] * 256 + nums[n - 1];
        // NAT-proof: connect to the host we dialled, not the (often private) address in the reply
        tokio::time::timeout(IO_TIMEOUT, TcpStream::connect((self.host.as_str(), port)))
            .await
            .map_err(|_| "data connection timed out".to_owned())?
            .map_err(|e| format!("data connection failed: {e}"))
    }

    async fn download(&mut self, path: &str) -> Result<Vec<u8>, String> {
        let mut d = self.data().await?;
        self.expect(&format!("RETR {}", safe(path)?), &[125, 150])
            .await?;
        let mut buf = Vec::new();
        d.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
        drop(d);
        let (code, text) = self.reply().await?;
        if code != 226 && code != 250 {
            return Err(format!("download failed: {text}"));
        }
        Ok(buf)
    }

    async fn upload(&mut self, path: &str, bytes: &[u8]) -> Result<(), String> {
        let mut d = self.data().await?;
        self.expect(&format!("STOR {}", safe(path)?), &[125, 150])
            .await?;
        d.write_all(bytes).await.map_err(|e| e.to_string())?;
        d.shutdown().await.map_err(|e| e.to_string())?;
        drop(d);
        let (code, text) = self.reply().await?;
        if code != 226 && code != 250 {
            return Err(format!("upload failed: {text}"));
        }
        Ok(())
    }

    /// Names in `path`; `MLSD` facts when the server has them, else `NLST` names.
    async fn list(&mut self, path: &str) -> Result<Vec<Value>, String> {
        for (verb, facts) in [("MLSD", true), ("NLST", false)] {
            let mut d = self.data().await?;
            let c = if path.is_empty() {
                verb.to_owned()
            } else {
                format!("{verb} {}", safe(path)?)
            };
            let (code, text) = self.cmd(&c).await?;
            if code != 125 && code != 150 {
                if facts {
                    continue; // no MLSD: try NLST
                }
                return Err(format!("{verb}: {text}"));
            }
            let mut buf = Vec::new();
            d.read_to_end(&mut buf).await.map_err(|e| e.to_string())?;
            drop(d);
            let _ = self.reply().await?;
            let text = String::from_utf8_lossy(&buf);
            let rows = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|l| {
                    if !facts {
                        return Some(json!({"name": l.trim(), "type": "unknown"}));
                    }
                    let (f, name) = l.split_once("; ")?;
                    let fact = |k: &str| {
                        f.split(';').find_map(|kv| {
                            kv.split_once('=')
                                .filter(|(a, _)| a.eq_ignore_ascii_case(k))
                                .map(|(_, v)| v.to_owned())
                        })
                    };
                    let ty = fact("type").unwrap_or_default().to_ascii_lowercase();
                    if ty == "cdir" || ty == "pdir" {
                        return None;
                    }
                    Some(json!({
                        "name": name.trim(),
                        "type": if ty == "dir" { "directory" } else { "file" },
                        "size": fact("size").and_then(|s| s.parse::<u64>().ok()),
                        "modified": fact("modify"),
                    }))
                })
                .collect();
            return Ok(rows);
        }
        Ok(Vec::new())
    }
}

#[derive(Debug)]
struct FtpNode {
    desc: NodeTypeDescription,
    creds: CredentialTypes,
}

#[async_trait]
impl FlowNode for FtpNode {
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
        let mut ftp = Ftp::connect(&cred)
            .await
            .map_err(|m| kit::err(name, format!("Could not connect: {m}")))?;
        let mut out = Vec::new();
        for i in 0..input_len {
            let op = kit::str_param(ctx, &self.desc, "operation", i)?;
            let path = kit::str_param(ctx, &self.desc, "path", i)?;
            let prop = kit::str_param(ctx, &self.desc, "binaryPropertyName", i)?;
            let prop = if prop.is_empty() {
                "data".to_owned()
            } else {
                prop
            };
            let res: Result<Vec<Item>, FlowError> = async {
                let e = |m: String| kit::err(name, m);
                Ok(match op.as_str() {
                    "list" => ftp
                        .list(&path)
                        .await
                        .map_err(e)?
                        .into_iter()
                        .map(kit::to_item)
                        .collect(),
                    "download" => {
                        let bytes = ftp.download(&path).await.map_err(e)?;
                        let file = path.rsplit('/').next().unwrap_or(&path).to_owned();
                        let mut it = Item::from_json(
                            json!({"path": path, "fileName": file, "fileSize": bytes.len()}),
                        );
                        let mut bin = hk_flow::BinaryData {
                            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                            mime_type: "application/octet-stream".into(),
                            file_name: Some(file),
                            ..hk_flow::BinaryData::default()
                        };
                        bin.file_type = None;
                        it.binary = Some([(prop.clone(), bin)].into_iter().collect());
                        vec![it]
                    }
                    "upload" => {
                        let f = kit::binary_file(ctx, &ctx.input[0][i], &prop)?;
                        ftp.upload(&path, &f.data).await.map_err(e)?;
                        vec![Item::from_json(
                            json!({"success": true, "path": path, "size": f.data.len()}),
                        )]
                    }
                    "delete" => {
                        ftp.expect(&format!("DELE {}", safe(&path).map_err(e)?), &[250])
                            .await
                            .map_err(e)?;
                        vec![Item::from_json(json!({"success": true, "path": path}))]
                    }
                    "rename" => {
                        let to = kit::str_param(ctx, &self.desc, "newPath", i)?;
                        ftp.expect(&format!("RNFR {}", safe(&path).map_err(e)?), &[350])
                            .await
                            .map_err(e)?;
                        ftp.expect(&format!("RNTO {}", safe(&to).map_err(e)?), &[250])
                            .await
                            .map_err(e)?;
                        vec![Item::from_json(
                            json!({"success": true, "from": path, "to": to}),
                        )]
                    }
                    "mkdir" => {
                        ftp.expect(&format!("MKD {}", safe(&path).map_err(e)?), &[257])
                            .await
                            .map_err(e)?;
                        vec![Item::from_json(json!({"success": true, "path": path}))]
                    }
                    other => return Err(kit::err(name, format!("Unknown operation `{other}`"))),
                })
            }
            .await;
            kit::settle(ctx, i, res, &mut out)?;
        }
        let _ = ftp.cmd("QUIT").await;
        Ok(vec![out])
    }
}

/// Node and credential type into the shared registries. Features: `int-ftp`.
pub fn register(nodes: &mut NodeRegistry, creds: &CredentialTypes) -> Result<(), FlowError> {
    creds.register_if_absent(credential_type());
    nodes.register(Arc::new(FtpNode {
        desc: description(),
        creds: creds.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_is_refused() {
        assert!(safe("/ok/file.txt").is_ok());
        assert!(safe("/x\r\nDELE /y").is_err());
    }
}

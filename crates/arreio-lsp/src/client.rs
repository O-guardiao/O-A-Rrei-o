use crate::protocol::*;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

/// Cliente LSP síncrono sobre stdio.
pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    id_counter: AtomicU64,
}

impl LspClient {
    /// Spawna o servidor LSP (ex: `rust-analyzer`).
    pub fn spawn(command: &str, args: &[&str]) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("falha ao spawnar LSP: {}", command))?;

        let stdin = child.stdin.take().context("stdin não disponível")?;
        Ok(Self {
            child,
            stdin,
            id_counter: AtomicU64::new(1),
        })
    }

    /// Envia initialize e aguarda resposta.
    pub fn initialize(&mut self, root_path: &str) -> Result<Value> {
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_path: Some(root_path.into()),
            capabilities: ClientCapabilities::default(),
        };
        self.request("initialize", Some(serde_json::to_value(params)?))
    }

    /// Solicita documentSymbol para um arquivo.
    pub fn document_symbol(&mut self, file_uri: &str) -> Result<Vec<DocumentSymbol>> {
        let params = DocumentSymbolParams {
            text_document: TextDocumentIdentifier {
                uri: file_uri.into(),
            },
        };
        let res = self.request(
            "textDocument/documentSymbol",
            Some(serde_json::to_value(params)?),
        )?;
        serde_json::from_value(res).context("falha ao parsear DocumentSymbol[]")
    }

    fn request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.id_counter.fetch_add(1, Ordering::SeqCst);
        let msg = JsonRpcMessage {
            jsonrpc: "2.0".into(),
            id: Some(id),
            method: Some(method.into()),
            params,
            result: None,
            error: None,
        };
        let json = serde_json::to_string(&msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", json.len());

        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(json.as_bytes())?;
        self.stdin.flush()?;

        // Lê resposta
        let stdout = self
            .child
            .stdout
            .as_mut()
            .context("stdout não disponível")?;
        let mut reader = BufReader::new(stdout);

        // Lê header Content-Length
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line.trim().is_empty() {
                break;
            }
            if line.to_lowercase().starts_with("content-length:") {
                content_length = line.split(':').nth(1).and_then(|s| s.trim().parse().ok());
            }
        }

        let len = content_length.context("Content-Length ausente na resposta LSP")?;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf)?;

        let resp: JsonRpcMessage<Value> = serde_json::from_slice(&buf)?;
        if let Some(err) = resp.error {
            bail!("LSP error ({}): {}", err.code, err.message);
        }
        resp.result.context("resposta sem result")
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        let _ = self.request("shutdown", None);
        let _ = self.request("exit", None);
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requer rust-analyzer instalado"]
    fn lsp_handshake_real() {
        let mut client = LspClient::spawn("rust-analyzer", &[]).unwrap();
        let res = client.initialize(".").unwrap();
        assert!(res.get("capabilities").is_some());
    }
}

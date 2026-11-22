use std::io::{self, BufReader};
use vela_lsp::{Server, Transport};

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut transport = Transport::new(BufReader::new(stdin.lock()), stdout.lock());
    let mut server = Server::new();

    while let Some(msg) = transport.read_message()? {
        if let Some(id) = msg.id {
            if let Some(method) = &msg.method {
                let response = server.handle_request(id, method, msg.params);
                transport.write_response(&response)?;
            }
        } else if let Some(method) = &msg.method {
            if method == "exit" {
                break;
            }
            if let Some(diagnostics) = server.handle_notification(method, msg.params) {
                let value = serde_json::to_value(&diagnostics)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                transport.write_notification("textDocument/publishDiagnostics", value)?;
            }
        }
    }

    Ok(())
}

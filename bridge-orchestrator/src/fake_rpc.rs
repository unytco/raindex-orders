//! An Ethereum JSON-RPC server over HTTP for tests, one request per connection.

use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Answers each call with what `answer` returns for its method and params, an
/// `Err` as the JSON-RPC error, and returns the server's URL.
pub async fn serve<A>(answer: A) -> String
where
    A: Fn(&str, &Value) -> Result<Value, Value> + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let answer = Arc::clone(&answer);
            tokio::spawn(async move { respond(socket, answer.as_ref()).await });
        }
    });
    url
}

async fn respond<A>(mut socket: TcpStream, answer: &A)
where
    A: Fn(&str, &Value) -> Result<Value, Value>,
{
    let body = read_request(&mut socket).await;
    let call: Value = serde_json::from_slice(&body).unwrap();
    let reply = match answer(call["method"].as_str().unwrap(), &call["params"]) {
        Ok(result) => json!({"jsonrpc": "2.0", "id": call["id"], "result": result}),
        Err(error) => json!({"jsonrpc": "2.0", "id": call["id"], "error": error}),
    }
    .to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    socket.write_all(response.as_bytes()).await.unwrap();
}

/// The body of the HTTP request on `socket`.
pub async fn read_request(socket: &mut TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    let body = loop {
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "connection closed mid-request");
        request.extend_from_slice(&chunk[..read]);
        let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .map(|value| value.trim().parse().unwrap())
            .unwrap_or(0);
        if request.len() >= end + 4 + length {
            break request[end + 4..end + 4 + length].to_vec();
        }
    };
    body
}

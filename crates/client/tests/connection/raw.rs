// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! A raw HTTP/1.1 peer that sends a scripted response, for server behaviour the mini server does not produce.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};

use protocol::{Frame, ServerMessage};

/// Encode `data` as one HTTP/1.1 chunk.
pub fn chunk(data: &[u8]) -> Vec<u8> {
    let mut output = format!("{:x}\r\n", data.len()).into_bytes();
    output.extend_from_slice(data);
    output.extend_from_slice(b"\r\n");
    output
}

pub fn message(message: &ServerMessage) -> Vec<u8> {
    chunk(&Frame::server(message).encode())
}

pub fn welcome() -> Vec<u8> {
    message(&ServerMessage::Welcome {
        protocol: protocol::PROTOCOL_VERSION,
        server: "mini".into(),
        source: 1,
    })
}

/// Accept connections forever. For each, read the request head and, if `body` is given, respond 200 with it as a chunked body; then hold the connection open until the client closes it.
pub fn start(body: Option<Vec<u8>>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let body = body.clone();
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(byte[0]);
                }
                if let Some(body) = body {
                    let mut response =
                        b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n".to_vec();
                    response.extend(body);
                    if stream.write_all(&response).is_err() {
                        return;
                    }
                }
                let mut sink = [0; 4096];
                while stream.read(&mut sink).unwrap_or(0) > 0 {}
            });
        }
    });
    address
}

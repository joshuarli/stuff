#![forbid(unsafe_code)]

use std::io::Read as _;
use std::sync::mpsc::{self, Sender};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use deser_value::Value;
use h12tiny_client_sync::{http, Client};

use crate::publish::{Environment, PublishError, Registry};
use crate::publish::service::PublishingService;
use crate::publish::r2::sigv4;

#[derive(Clone)]
pub struct Coordinator {
    endpoint: String,
    token: String,
    registry: Registry,
}

impl std::fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Coordinator").field("endpoint", &self.endpoint).field("registry", &self.registry).finish_non_exhaustive()
    }
}

pub struct Lease {
    coordinator: Coordinator,
    pub token: String,
    pub revision: u64,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
    renewal_failed: Arc<AtomicBool>,
}

impl Coordinator {
    pub fn configured(env: &Environment, registry: Registry) -> Result<Option<Self>, PublishError> {
        Ok(PublishingService::configured(env)?.map(|service| Self { endpoint: service.endpoint, token: service.coordinator_token, registry }))
    }

    fn request(&self, operation: &str, body: Option<&str>) -> Result<Value, PublishError> {
        let url = format!("{}/_publication/{}/{}", self.endpoint, self.registry.as_str(), operation);
        let request = http::Request::builder()
            .method(if body.is_some() { "POST" } else { "GET" })
            .uri(&url)
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/json")
            .body(body.unwrap_or("").as_bytes().to_vec())
            .map_err(|e| PublishError::Config(format!("coordinator request: {e}")))?;
        let client = Client::builder().connect_timeout(Duration::from_secs(10)).build();
        let mut response = client.request_with_timeout(request, Some(Duration::from_secs(30)))
            .map_err(|e| PublishError::Transport(format!("coordinator {operation}: {e}")))?;
        let status = response.status().as_u16();
        let mut bytes = Vec::new();
        response.body_mut().take(65_537).read_to_end(&mut bytes)
            .map_err(|e| PublishError::Transport(format!("coordinator {operation}: {e}")))?;
        if bytes.len() > 65_536 { return Err(PublishError::Integrity("coordinator response exceeds 64 KiB".into())); }
        if status == 409 { return Err(PublishError::Conflict(format!("coordinator {operation} conflicts with current publication state"))); }
        if !(200..300).contains(&status) { return Err(PublishError::Transport(format!("coordinator {operation}: HTTP {status}"))); }
        deser_json::from_slice(&bytes).map_err(|e| PublishError::Integrity(format!("coordinator {operation}: {e}")))
    }

    pub fn current(&self) -> Result<(u64, Option<String>), PublishError> {
        let value = self.request("current", None)?;
        let map = value.as_map().ok_or_else(|| PublishError::Integrity("invalid coordinator current".into()))?;
        let revision = map.get("revision").and_then(|v| v.kind().as_u64()).ok_or_else(|| PublishError::Integrity("invalid coordinator revision".into()))?;
        let generation = map.get("generation").and_then(|v| v.kind().as_str()).map(str::to_string);
        Ok((revision, generation))
    }

    pub fn begin(&self) -> Result<Lease, PublishError> {
        let value = self.request("begin", Some("{}"))?;
        let map = value.as_map().ok_or_else(|| PublishError::Integrity("invalid coordinator lease".into()))?;
        let token = map.get("token").and_then(|v| v.kind().as_str()).ok_or_else(|| PublishError::Integrity("missing lease token".into()))?.to_string();
        let revision = map.get("revision").and_then(|v| v.kind().as_u64()).ok_or_else(|| PublishError::Integrity("missing lease revision".into()))?;
        let coordinator = self.clone();
        let renewal = self.clone();
        let renewal_token = token.clone();
        if !token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') || token.is_empty() || token.len() > 128 {
            return Err(PublishError::Integrity("invalid lease token".into()));
        }
        let renewal_failed = Arc::new(AtomicBool::new(false));
        let failure = Arc::clone(&renewal_failed);
        let (stop, rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            while matches!(rx.recv_timeout(Duration::from_secs(60)), Err(mpsc::RecvTimeoutError::Timeout)) {
                if renewal.request("renew", Some(&format!("{{\"token\":\"{renewal_token}\"}}"))).is_err() {
                    failure.store(true, Ordering::Release);
                    break;
                }
            }
        });
        Ok(Lease { coordinator, token, revision, stop: Some(stop), thread: Some(thread), renewal_failed })
    }
}

impl Lease {
    pub fn activate(&self, operation: &str, generation: &str, manifest: &[u8]) -> Result<(), PublishError> {
        if self.renewal_failed.load(Ordering::Acquire) {
            return Err(PublishError::Conflict("coordinator lease renewal failed".into()));
        }
        crate::publish::r2::promote::validate_generation(generation)?;
        let hash = sigv4::sha256_hex(manifest);
        let identity = sigv4::sha256_hex(format!("{}:{operation}:{generation}:{hash}:{}", self.coordinator.registry.as_str(), self.revision).as_bytes());
        let body = format!("{{\"token\":\"{}\",\"key\":\"{}\",\"generation\":\"{}\",\"manifest\":\"{}\",\"expectedRevision\":{}}}", self.token, identity, generation, hash, self.revision);
        match self.coordinator.request(operation, Some(&body)) {
            Err(PublishError::Transport(_)) => { self.coordinator.request(operation, Some(&body))?; }
            result => { result?; }
        }
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() { let _ = stop.send(()); }
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
        let _ = self.coordinator.request("abort", Some(&format!("{{\"token\":\"{}\"}}", self.token)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    #[test]
    fn lost_activation_response_replays_the_same_receipt_key() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let mut activation = None;
            for index in 0..4 {
                let (mut socket, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_string();
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" { break; }
                    if let Some((name, value)) = line.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") { length = value.trim().parse().unwrap(); }
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                if index == 1 {
                    assert!(path.ends_with("/activate"));
                    activation = Some(body);
                    continue;
                }
                if index == 2 {
                    assert_eq!(activation.as_ref(), Some(&body));
                    assert!(path.ends_with("/activate"));
                }
                let reply = if index == 0 { r#"{"token":"lease-1","revision":7,"expires":100}"# } else { r#"{"ok":true}"# };
                write!(socket, "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}", reply.len(), reply).unwrap();
            }
        });
        let coordinator = Coordinator { endpoint, token: "service".into(), registry: Registry::Cargo };
        let lease = coordinator.begin().unwrap();
        assert_eq!(lease.revision, 7);
        lease.activate("activate", "generation-1", b"manifest").unwrap();
        drop(lease);
        server.join().unwrap();
    }
}

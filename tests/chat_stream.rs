//! `chat::ask_streaming` contra una instancia simulada: encolar, leer fragmentos por SSE,
//! retomar con el cursor si el flujo se corta, y detectar un fallo del worker por el
//! estado del mensaje (la instancia no cierra el flujo cuando falla).

use iurefficient_connect::chat::{self, Request, Scope};
use iurefficient_connect::rest::Session;
use iurefficient_connect::Account;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

type Log = Arc<Mutex<Vec<String>>>;

/// Servidor HTTP mínimo: una conexión por petición; `route` decide la respuesta cruda.
async fn serve(route: fn(&str) -> String) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Log = Arc::default();
    let seen = log.clone();
    tokio::spawn(async move {
        loop {
            let (mut sock, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or_default().to_string();
                seen.lock().unwrap().push(line.clone());
                let resp = route(&line);
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (format!("http://{addr}"), log)
}

fn json(status: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sse(body: &str) -> String {
    format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}")
}

fn session(base: &str) -> Session {
    Session::new(Account::new(base, "a@b.mx").unwrap(), "test/1").unwrap()
}

#[tokio::test]
async fn transmite_retoma_y_devuelve_la_respuesta_final() {
    let (base, log) = serve(|line| {
        if line.starts_with("POST /api/chat/global/chat/async") {
            json("202 Accepted", r#"{"user_message":{"id":"u1"},"job_id":"j1"}"#)
        } else if line.contains("/stream?cursor=0") {
            // Se corta sin `done`: el cliente debe reconectar desde el cursor 1.
            sse("id: 1\nevent: token\ndata: {\"text\": \"Hola\"}\n\n")
        } else if line.contains("/stream?cursor=1") {
            sse("id: 2\nevent: token\ndata: {\"text\": \" señor\"}\n\nevent: done\ndata: {\"status\": \"completed\"}\n\n")
        } else if line.contains("/messages/u1/status") {
            json(
                "200 OK",
                r#"{"status":"completed","assistant_message":{"id":"a1","content":"Hola señor [1]","model":"m","references":[{"index":1,"type":"document","title":"Contrato","excerpt":"","entity_id":"d1"}]}}"#,
            )
        } else {
            json("404 Not Found", r#"{"error":"no"}"#)
        }
    })
    .await;
    let sess = session(&base);
    let mut tokens = String::new();
    let req = Request { message: "hola", conversation_id: Some("c1"), ..Default::default() };
    let answer = chat::ask_streaming(&sess, Scope::Global, &req, |t| tokens.push_str(t)).await.unwrap();
    assert_eq!(tokens, "Hola señor");
    assert_eq!(answer.content, "Hola señor [1]");
    assert_eq!(answer.references[0].entity_id.as_deref(), Some("d1"));
    let log = log.lock().unwrap();
    assert!(log.iter().any(|l| l.contains("stream?cursor=1")), "{log:?}");
}

#[tokio::test]
async fn un_fallo_del_worker_se_detecta_con_el_latido() {
    let (base, _log) = serve(|line| {
        if line.starts_with("POST /api/chat/cases/k1/chat/async") {
            json("202 Accepted", r#"{"user_message":{"id":"u2"}}"#)
        } else if line.contains("/stream") {
            sse("id: 1\nevent: token\ndata: {\"text\": \"Empiezo\"}\n\n: latido\n\n")
        } else if line.contains("/messages/u2/status") {
            json("200 OK", r#"{"status":"failed","message":{"id":"u2","error_message":"modelo caído"}}"#)
        } else {
            json("404 Not Found", r#"{"error":"no"}"#)
        }
    })
    .await;
    let sess = session(&base);
    let req = Request { message: "hola", ..Default::default() };
    let err = chat::ask_streaming(&sess, Scope::Case("k1"), &req, |_| {}).await.unwrap_err();
    assert!(err.to_string().contains("modelo caído"), "{err}");
}

#[tokio::test]
async fn la_cuota_agotada_llega_como_error_de_la_instancia() {
    let (base, _log) = serve(|_| json("403 Forbidden", r#"{"error":"Límite mensual de IA alcanzado","quota_exceeded":"ai_requests"}"#)).await;
    let sess = session(&base);
    let req = Request { message: "hola", ..Default::default() };
    let err = chat::ask_streaming(&sess, Scope::Global, &req, |_| {}).await.unwrap_err();
    let api = err.downcast_ref::<iurefficient_connect::rest::ApiError>().expect("ApiError");
    assert_eq!(api.status, 403);
    assert!(api.is_quota_exceeded());
    assert!(api.message.contains("Límite mensual"));
}

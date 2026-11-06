//! Chat con el asistente de IA de la instancia, con respuesta en streaming.
//!
//! El chat corre en un worker (Celery) de la instancia: se encola con
//! `POST …/chat/async` y los fragmentos que el modelo va generando se leen por SSE en
//! `GET /api/chat/messages/<id>/stream`. Al terminar, la respuesta definitiva (ya
//! post-procesada: avisos de validación, fuentes) se relee de
//! `GET /api/chat/messages/<id>/status`; los fragmentos sólo sirven para mostrar el
//! texto mientras se escribe.
//!
//! **Usa siempre una conversación** ([`new_conversation`]): sin `conversation_id`, la
//! instancia toma como historial TODO el chat global del usuario, o en un proyecto todo
//! el chat del proyecto (de todos sus miembros).
//!
//! Si el worker falla, la instancia marca el mensaje como `failed` pero no cierra el
//! flujo SSE (esperaría hasta su tope, 5 min): por eso, con cada latido del flujo
//! (cada ~15 s sin fragmentos) se consulta también el estado del mensaje.

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use reqwest::{header, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

use crate::api::{opt, s};
use crate::rest::{api_error, Session};
use crate::tr;

/// Dónde se pregunta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Chat general del usuario (sin proyecto).
    Global,
    /// Chat de un proyecto: busca en sus documentos y cita las fuentes.
    Case(&'a str),
}

impl Scope<'_> {
    fn base(&self) -> String {
        match self {
            Scope::Global => "/api/chat/global".into(),
            Scope::Case(id) => format!("/api/chat/cases/{id}"),
        }
    }
}

/// Crea una conversación y devuelve su id. El título se ve en la web de la instancia.
pub async fn new_conversation(sess: &Session, scope: Scope<'_>, title: &str) -> Result<String> {
    let v = sess
        .post_json(&format!("{}/conversations", scope.base()), &json!({ "title": title }))
        .await?;
    v["conversation"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!(tr!("The instance did not create the conversation", "La instancia no creó la conversación")))
}

/// Borra (desactiva) una conversación. Una que ya no existe no es error.
pub async fn delete_conversation(sess: &Session, scope: Scope<'_>, conversation_id: &str) -> Result<()> {
    match sess
        .delete_json(&format!("{}/conversations/{conversation_id}", scope.base()))
        .await
    {
        Ok(_) => Ok(()),
        Err(e) if e.downcast_ref::<crate::rest::ApiError>().is_some_and(|a| a.status == 404) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Una pregunta al asistente.
#[derive(Debug, Clone, Default)]
pub struct Request<'a> {
    pub message: &'a str,
    /// Instrucciones propias (se combinan con las de la instancia y, en un proyecto,
    /// con la persona que imponga su pack de rol).
    pub system_prompt: Option<&'a str>,
    pub conversation_id: Option<&'a str>,
    /// Documentos de la instancia como contexto (sólo chat global).
    pub document_ids: &'a [String],
    /// Responder con el modelo potente («análisis profundo») en vez del ágil.
    pub deep_analysis: bool,
}

/// Fuente citada en una respuesta (`[1]`, `[2]`… en el texto).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Reference {
    pub index: u64,
    /// `document`, `task`, `deadline`…
    pub kind: String,
    pub title: String,
    pub excerpt: String,
    /// Id del documento, tarea, plazo… al que remite.
    pub entity_id: Option<String>,
}

/// Respuesta definitiva del asistente.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Answer {
    pub content: String,
    pub references: Vec<Reference>,
    pub model: Option<String>,
    /// Id del mensaje del asistente en la instancia.
    pub message_id: String,
}

fn answer_from(msg: &Value) -> Result<Answer> {
    let content = s(msg, "content").trim().to_string();
    if content.is_empty() {
        return Err(anyhow!(tr!("The instance returned no response", "La instancia no devolvió respuesta")));
    }
    let references = msg["references"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| Reference {
                    index: r["index"].as_u64().unwrap_or(0),
                    kind: s(r, "type"),
                    title: s(r, "title"),
                    excerpt: s(r, "excerpt"),
                    entity_id: opt(r, "entity_id"),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Answer { content, references, model: opt(msg, "model"), message_id: s(msg, "id") })
}

fn body_of(req: &Request<'_>, scope: Scope<'_>) -> Value {
    let mut body = json!({
        "message": req.message,
        "enable_url_extraction": false,
        "deep_analysis": req.deep_analysis,
    });
    if let Some(c) = req.conversation_id {
        body["conversation_id"] = json!(c);
    }
    if let Some(sp) = req.system_prompt.filter(|s| !s.trim().is_empty()) {
        body["system_prompt"] = json!(sp);
    }
    if scope == Scope::Global && !req.document_ids.is_empty() {
        body["document_ids"] = json!(req.document_ids);
    }
    body
}

/// Pregunta y espera la respuesta completa, sin streaming (`POST …/chat`).
pub async fn ask(sess: &Session, scope: Scope<'_>, req: &Request<'_>) -> Result<Answer> {
    let v = sess.post_json(&format!("{}/chat", scope.base()), &body_of(req, scope)).await?;
    answer_from(&v["assistant_message"])
}

/// Pregunta y va entregando a `on_token` cada fragmento de la respuesta según la escribe
/// el modelo. Devuelve la respuesta definitiva, que puede diferir de la suma de los
/// fragmentos (la instancia la post-procesa).
///
/// Cancelar es soltar el futuro: la instancia termina la respuesta igual (y la cobra),
/// pero ya no se lee.
pub async fn ask_streaming(
    sess: &Session,
    scope: Scope<'_>,
    req: &Request<'_>,
    mut on_token: impl FnMut(&str),
) -> Result<Answer> {
    let queued = sess
        .post_json(&format!("{}/chat/async", scope.base()), &body_of(req, scope))
        .await?;
    let message_id = s(&queued["user_message"], "id");
    if message_id.is_empty() {
        return Err(anyhow!(tr!("The instance did not queue the question", "La instancia no encoló la pregunta")));
    }

    let mut cursor: u64 = 0;
    let mut reconnects = 0;
    // `Some(final)` cuando el flujo ya cerró (o el estado lo dio por terminado).
    let mut finished: Option<Answer> = None;
    'stream: while finished.is_none() {
        let resp = open_stream(sess, &message_id, cursor).await?;
        let mut body = resp.bytes_stream();
        let mut parser = SseParser::default();
        // Estado consultado con el primer latido y luego cada 10 s como mucho.
        let mut last_check: Option<Instant> = None;
        while let Some(chunk) = body.next().await {
            let Ok(chunk) = chunk else { break };
            for ev in parser.push_bytes(&chunk) {
                match ev {
                    SseEvent::Token { id, text } => {
                        if let Some(id) = id {
                            cursor = id;
                        }
                        on_token(&text);
                    }
                    SseEvent::Done { status } => {
                        if status == "completed" {
                            break 'stream;
                        }
                        // `timeout` u otro: el estado dice si terminó o falló.
                        if let Some(a) = check_status(sess, &message_id).await? {
                            finished = Some(a);
                        }
                        break 'stream;
                    }
                    SseEvent::Heartbeat => {
                        if last_check.map_or(true, |t| t.elapsed() > Duration::from_secs(10)) {
                            last_check = Some(Instant::now());
                            if let Some(a) = check_status(sess, &message_id).await? {
                                finished = Some(a);
                                break 'stream;
                            }
                        }
                    }
                }
            }
        }
        // El flujo se cortó sin `done` (red, proxy): se retoma desde el cursor.
        reconnects += 1;
        if reconnects > 3 {
            break;
        }
    }
    match finished {
        Some(a) => Ok(a),
        None => wait_final(sess, &message_id).await,
    }
}

async fn open_stream(sess: &Session, message_id: &str, cursor: u64) -> Result<reqwest::Response> {
    let path = format!("/api/chat/messages/{message_id}/stream?cursor={cursor}");
    let send = || async {
        sess.request(Method::GET, &path)
            .await?
            .header(header::ACCEPT, "text/event-stream")
            .send()
            .await
            .map_err(anyhow::Error::from)
    };
    let mut resp = send().await?;
    if resp.status() == StatusCode::UNAUTHORIZED && sess.has_refresh() && sess.refresh().await.is_ok() {
        resp = send().await?;
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(api_error(status, &text).into());
    }
    Ok(resp)
}

/// Estado del mensaje: `Some` si ya terminó, `None` si sigue en curso; error si falló.
async fn check_status(sess: &Session, message_id: &str) -> Result<Option<Answer>> {
    let v = sess.get_json(&format!("/api/chat/messages/{message_id}/status")).await?;
    match v["status"].as_str().unwrap_or_default() {
        "completed" if v["assistant_message"].is_object() => answer_from(&v["assistant_message"]).map(Some),
        "failed" => {
            let detail = s(&v["message"], "error_message");
            Err(anyhow!(if detail.is_empty() {
                tr!("The instance could not answer", "La instancia no pudo responder")
            } else {
                tr!("The instance could not answer: {detail}", "La instancia no pudo responder: {detail}")
            }))
        }
        _ => Ok(None),
    }
}

/// Tras el cierre del flujo, la respuesta guardada puede tardar un instante en estar.
async fn wait_final(sess: &Session, message_id: &str) -> Result<Answer> {
    let start = Instant::now();
    loop {
        if let Some(a) = check_status(sess, message_id).await? {
            return Ok(a);
        }
        if start.elapsed() > Duration::from_secs(600) {
            return Err(anyhow!(tr!("The instance took too long to answer", "La instancia tardó demasiado en responder")));
        }
        let wait = if start.elapsed() < Duration::from_secs(10) { 500 } else { 2000 };
        tokio::time::sleep(Duration::from_millis(wait)).await;
    }
}

// ---------------------------------------------------------------------------
// SSE
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum SseEvent {
    /// Fragmento de texto; `id` es el cursor para retomar tras él.
    Token { id: Option<u64>, text: String },
    Done { status: String },
    /// Comentario (`: latido`): la conexión sigue viva sin fragmentos nuevos.
    Heartbeat,
}

/// Lector incremental de eventos SSE: los bytes llegan en trozos arbitrarios (un carácter
/// UTF-8 puede quedar partido entre dos) y un evento termina en una línea en blanco.
#[derive(Default)]
struct SseParser {
    buf: String,
    /// Bytes del final de un trozo que aún no forman un carácter completo.
    pending: Vec<u8>,
}

impl SseParser {
    fn push_bytes(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.pending.extend_from_slice(chunk);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            // Secuencia incompleta al final: se guarda para el siguiente trozo. Un byte
            // realmente inválido (no al final) se reemplaza para no atascar el flujo.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => {
                let text = String::from_utf8_lossy(&self.pending).into_owned();
                self.pending.clear();
                return self.push(&text);
            }
        };
        let rest = self.pending.split_off(valid);
        let text = String::from_utf8(std::mem::replace(&mut self.pending, rest)).unwrap_or_default();
        self.push(&text)
    }

    fn push(&mut self, chunk: &str) -> Vec<SseEvent> {
        self.buf.push_str(&chunk.replace("\r\n", "\n"));
        let mut out = Vec::new();
        while let Some(end) = self.buf.find("\n\n") {
            let raw: String = self.buf.drain(..end + 2).collect();
            if let Some(ev) = parse_event(&raw) {
                out.push(ev);
            }
        }
        out
    }
}

fn parse_event(raw: &str) -> Option<SseEvent> {
    let (mut id, mut event, mut data, mut comment) = (None, String::new(), String::new(), false);
    for line in raw.lines() {
        if line.starts_with(':') {
            comment = true;
        } else if let Some(v) = line.strip_prefix("id:") {
            id = v.trim().parse().ok();
        } else if let Some(v) = line.strip_prefix("event:") {
            event = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    let payload: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
    match event.as_str() {
        "token" => Some(SseEvent::Token { id, text: s(&payload, "text") }),
        "done" => Some(SseEvent::Done { status: s(&payload, "status") }),
        "" if comment => Some(SseEvent::Heartbeat),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_en_trozos_arbitrarios() {
        let mut p = SseParser::default();
        assert!(p.push("id: 1\nevent: tok").is_empty());
        let evs = p.push("en\ndata: {\"text\": \"Hola\"}\n\nid: 2\nevent: token\ndata: {\"text\": \" mundo\\n\"}\n\n: latido\n\n");
        assert_eq!(
            evs,
            vec![
                SseEvent::Token { id: Some(1), text: "Hola".into() },
                SseEvent::Token { id: Some(2), text: " mundo\n".into() },
                SseEvent::Heartbeat,
            ]
        );
        assert_eq!(
            p.push("event: done\r\ndata: {\"status\": \"completed\"}\r\n\r\n"),
            vec![SseEvent::Done { status: "completed".into() }]
        );
    }

    #[test]
    fn utf8_partido_entre_trozos() {
        let mut p = SseParser::default();
        let bytes = "event: token\ndata: {\"text\": \"señor\"}\n\n".as_bytes();
        let cut = bytes.iter().position(|&b| b == 0xC3).unwrap() + 1; // a mitad de la «ñ»
        assert!(p.push_bytes(&bytes[..cut]).is_empty());
        assert_eq!(p.push_bytes(&bytes[cut..]), vec![SseEvent::Token { id: None, text: "señor".into() }]);
    }

    #[test]
    fn eventos_desconocidos_se_ignoran() {
        let mut p = SseParser::default();
        assert!(p.push("event: progress\ndata: {}\n\n").is_empty());
    }

    #[test]
    fn respuesta_con_fuentes() {
        let msg = json!({
            "id": "m1", "content": " Texto [1] ", "model": "x/y",
            "references": [{"index": 1, "type": "document", "title": "Contrato", "excerpt": "Type: contract", "entity_id": "d9"}]
        });
        let a = answer_from(&msg).unwrap();
        assert_eq!(a.content, "Texto [1]");
        assert_eq!(a.references[0].entity_id.as_deref(), Some("d9"));
        assert_eq!(a.message_id, "m1");
    }

    #[test]
    fn cuerpo_sin_documentos_en_proyecto() {
        let ids = vec!["d1".to_string()];
        let req = Request { message: "hola", conversation_id: Some("c1"), document_ids: &ids, ..Default::default() };
        assert!(body_of(&req, Scope::Global).get("document_ids").is_some());
        let b = body_of(&req, Scope::Case("k1"));
        assert!(b.get("document_ids").is_none());
        assert_eq!(b["conversation_id"], "c1");
        assert!(b.get("system_prompt").is_none());
    }
}

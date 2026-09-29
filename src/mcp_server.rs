//! Servidor MCP local por stdio (`<App> --mcp`), común a las apps de escritorio.
//!
//! Claude Desktop, Claude Code, Copilot en VS Code o cualquier cliente MCP lanza el
//! ejecutable de la app con `--mcp` y le habla por stdin/stdout: JSON-RPC 2.0, un mensaje
//! por línea (transporte stdio de la spec). Este módulo pone el protocolo —negociación,
//! `tools/list`, cancelación, progreso— y cada app sólo describe sus herramientas con
//! [`Handler`].
//!
//! Stdout es del protocolo: cualquier otra cosa escrita ahí rompe al cliente. El registro
//! tiene que ir a stderr o a un archivo.
//!
//! Cada `tools/call` corre en su propio hilo, para que `notifications/cancelled` pueda
//! detener un trabajo largo mientras tanto; si la app necesita que algo vaya de uno en
//! uno, lo serializa ella.
//!
//! No confundir con [`crate::mcp`], el cliente del servidor MCP de la instancia.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Versiones de la spec que este servidor sabe hablar; la primera es la preferida.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const PARSE_ERROR: i64 = -32700;

/// Lo que el servidor dice de sí mismo en `initialize`.
pub struct ServerInfo {
    /// Identificador (`iureocr`).
    pub name: &'static str,
    /// Nombre visible (`IureOCR`).
    pub title: &'static str,
    pub version: &'static str,
    /// Indicaciones para el modelo: cuándo usar las herramientas y cómo.
    pub instructions: String,
}

/// Resultado de una herramienta.
pub enum Reply {
    /// Datos: se envían como `structuredContent` y, para los clientes que aún no lo leen,
    /// como texto JSON.
    Data(Value),
    /// Un `result` de `tools/call` ya armado (p. ej. contenido de imagen).
    Raw(Value),
}

/// Error de una herramienta.
pub enum CallError {
    /// La herramienta falló: el modelo recibe el mensaje (`isError: true`) y puede
    /// decírselo al usuario o corregir la llamada.
    Tool(anyhow::Error),
    /// Llamada mal formada (herramienta inexistente…): error de protocolo.
    Params(String),
}

impl From<anyhow::Error> for CallError {
    fn from(e: anyhow::Error) -> Self {
        CallError::Tool(e)
    }
}

/// Contexto de una llamada: señal de cancelación y notificaciones de progreso.
pub struct Call<'a> {
    pub cancel: &'a AtomicBool,
    progress_token: Option<Value>,
    out: &'a Output,
}

impl Call<'_> {
    /// Envía `notifications/progress` si el cliente lo pidió (`_meta.progressToken`).
    pub fn progress(&self, done: f64, total: Option<f64>, message: &str) {
        let Some(token) = &self.progress_token else { return };
        let mut params = json!({"progressToken": token, "progress": done, "message": message});
        if let Some(t) = total {
            params["total"] = json!(t);
        }
        self.out.send(&json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// Las herramientas de una app.
pub trait Handler: Send + Sync + 'static {
    fn info(&self) -> ServerInfo;
    /// La lista de `tools/list` (nombre, descripción, `inputSchema`, `annotations`…).
    fn tools(&self) -> Value;
    fn call(&self, name: &str, args: &Value, call: &Call) -> Result<Reply, CallError>;
}

struct Output(Mutex<std::io::Stdout>);

impl Output {
    fn send(&self, msg: &Value) {
        let text = msg.to_string();
        let mut out = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// El `result` de una herramienta que falló.
pub fn tool_error(e: &anyhow::Error) -> Value {
    json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true})
}

/// Los primeros `max` caracteres de `text` y si hubo recorte.
pub fn cut(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((i, _)) => (text[..i].to_string(), true),
        None => (text.to_string(), false),
    }
}

struct Server<H> {
    handler: H,
    out: Output,
    /// Llamadas en curso: id JSON-RPC (serializado) → señal de cancelación.
    running: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

/// Atiende stdin hasta que el cliente lo cierra. Devuelve el código de salida.
pub fn serve<H: Handler>(handler: H) -> i32 {
    let server = Arc::new(Server { handler, out: Output(Mutex::new(std::io::stdout())), running: Mutex::new(HashMap::new()) });
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(msg) => server.clone().handle(msg),
            Err(e) => server.out.send(&error(Value::Null, PARSE_ERROR, &format!("JSON: {e}"))),
        }
    }
    // stdin cerrado: el cliente se fue. Los hilos que queden mueren con el proceso.
    log::info!("MCP: el cliente cerró la conexión");
    0
}

impl<H: Handler> Server<H> {
    fn handle(self: Arc<Self>, msg: Value) {
        let Some(obj) = msg.as_object() else {
            // Un lote (array) no lo manda ningún cliente de stdio actual.
            self.out.send(&error(Value::Null, INVALID_REQUEST, "batches are not supported"));
            return;
        };
        let method = obj.get("method").and_then(Value::as_str).unwrap_or("");
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = obj.get("id").cloned().filter(|v| !v.is_null()) else {
            // Notificación (o respuesta a algo que no pedimos): nunca se contesta.
            if method == "notifications/cancelled" {
                if let Some(req) = params.get("requestId") {
                    if let Some(c) = self.running.lock().unwrap().get(&req.to_string()) {
                        log::info!("MCP: cancelada la llamada {req}");
                        c.store(true, Ordering::Relaxed);
                    }
                }
            }
            return;
        };
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
                let client = params.pointer("/clientInfo/name").and_then(Value::as_str).unwrap_or("?");
                log::info!("MCP: initialize de {client} (protocolo {version})");
                let info = self.handler.info();
                self.out.send(&result(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": info.name, "title": info.title, "version": info.version},
                        "instructions": info.instructions,
                    }),
                ));
            }
            "ping" => self.out.send(&result(id, json!({}))),
            "tools/list" => self.out.send(&result(id, json!({"tools": self.handler.tools()}))),
            "tools/call" => {
                let key = id.to_string();
                let cancel = Arc::new(AtomicBool::new(false));
                self.running.lock().unwrap().insert(key.clone(), cancel.clone());
                let this = self.clone();
                std::thread::spawn(move || {
                    let reply = this.call(&params, &cancel);
                    this.running.lock().unwrap().remove(&key);
                    // La spec pide no contestar una petición cancelada.
                    if cancel.load(Ordering::Relaxed) {
                        return;
                    }
                    this.out.send(&match reply {
                        Ok(v) => result(id, v),
                        Err(m) => error(id, INVALID_PARAMS, &m),
                    });
                });
            }
            other => self.out.send(&error(id, METHOD_NOT_FOUND, &format!("method not supported: {other}"))),
        }
    }

    fn call(&self, params: &Value, cancel: &AtomicBool) -> Result<Value, String> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if !args.is_object() {
            return Err("arguments must be an object".into());
        }
        log::info!("MCP: {name} {}", cut(&args.to_string(), 300).0);
        let call = Call { cancel, progress_token: params.pointer("/_meta/progressToken").cloned(), out: &self.out };
        match self.handler.call(name, &args, &call) {
            Ok(Reply::Data(v)) => Ok(json!({
                "content": [{"type": "text", "text": v.to_string()}],
                "structuredContent": v,
                "isError": false,
            })),
            Ok(Reply::Raw(v)) => Ok(v),
            Err(CallError::Tool(e)) => {
                log::warn!("MCP: {name}: {e:#}");
                Ok(tool_error(&e))
            }
            Err(CallError::Params(m)) => Err(m),
        }
    }
}

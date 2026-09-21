//! Cliente mínimo del servidor MCP de Iurefficient (`/api/mcp`, Streamable HTTP sin
//! sesión). Sólo lectura: buscar en expedientes, listar proyectos, tareas, vencimientos.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Account;

const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Value,
}

pub struct McpClient {
    acc: Account,
    token: String,
    http: reqwest::Client,
    seq: AtomicU64,
}

impl McpClient {
    pub fn new(acc: Account, token: &str, user_agent: &str) -> Result<Self> {
        if !token.starts_with("iurmcp_") {
            return Err(anyhow!("El token MCP debe empezar con iurmcp_"));
        }
        let http = reqwest::Client::builder().user_agent(user_agent).timeout(std::time::Duration::from_secs(60)).build()?;
        Ok(Self { acc, token: token.to_string(), http, seq: AtomicU64::new(1) })
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.seq.fetch_add(1, Ordering::Relaxed);
        let url = self.acc.api("/api/mcp")?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .context("No se pudo conectar con el servidor MCP")?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(anyhow!("Token MCP rechazado (401)"));
        }
        if !status.is_success() {
            return Err(anyhow!("El servidor MCP respondió {status}"));
        }
        let v: Value = resp.json().await.context("Respuesta MCP no válida")?;
        if let Some(err) = v.get("error") {
            return Err(anyhow!("MCP: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or("error")));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// `initialize`: devuelve el nombre del servidor y las instrucciones.
    pub async fn initialize(&self, client_name: &str, client_version: &str) -> Result<Value> {
        self.rpc(
            "initialize",
            json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": {"name": client_name, "version": client_version}}),
        )
        .await
    }

    pub async fn tools(&self) -> Result<Vec<Tool>> {
        let r = self.rpc("tools/list", json!({})).await?;
        Ok(serde_json::from_value(r.get("tools").cloned().unwrap_or(json!([]))).unwrap_or_default())
    }

    /// Llama a una tool y devuelve `structuredContent` si existe; si no, el texto
    /// del primer bloque de contenido interpretado como JSON o como cadena.
    pub async fn call(&self, name: &str, arguments: Value) -> Result<Value> {
        let r = self.rpc("tools/call", json!({"name": name, "arguments": arguments})).await?;
        if r.get("isError").and_then(|b| b.as_bool()).unwrap_or(false) {
            let msg = r["content"].as_array().and_then(|c| c.first()).and_then(|c| c["text"].as_str()).unwrap_or("error");
            return Err(anyhow!("MCP {name}: {msg}"));
        }
        if let Some(sc) = r.get("structuredContent") {
            return Ok(sc.clone());
        }
        let text = r["content"].as_array().and_then(|c| c.first()).and_then(|c| c["text"].as_str()).unwrap_or("");
        Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string())))
    }

    pub async fn listar_proyectos(&self, busqueda: Option<&str>, limite: u32) -> Result<Value> {
        let mut args = json!({"limite": limite.clamp(1, 50)});
        if let Some(b) = busqueda.filter(|b| !b.trim().is_empty()) {
            args["busqueda"] = json!(b.trim());
        }
        self.call("listar_proyectos", args).await
    }

    pub async fn buscar_en_expedientes(&self, query: &str, limite: u32) -> Result<Value> {
        self.call("buscar_en_expedientes", json!({"query": query, "limite": limite.clamp(1, 20)})).await
    }
}

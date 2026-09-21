//! Conector común de las aplicaciones de escritorio de Iurefficient
//! (IureTranscribe, IureEditor, IureDav) con una instancia.
//!
//! - [`account`]: identidad de la instancia (dominio normalizado) y del usuario.
//! - [`secrets`]: credenciales en el llavero del sistema, compartidas entre apps.
//! - [`webdav`]: árbol de documentos (listar, subir, descargar) con contraseña `iurdav_…`.
//! - [`mcp`]: servidor MCP de sólo lectura con token `iurmcp_…`.
//! - [`rest`]: API REST con sesión de cookies (JWT + CSRF + refresco).
//! - [`releases`]: aviso de versiones nuevas publicadas en GitHub.
//!
//! Sin dependencias de Tauri ni de ninguna interfaz: cada app pone su propia UI encima.

pub mod account;
pub mod mcp;
pub mod releases;
pub mod rest;
#[cfg(feature = "keyring")]
pub mod secrets;
pub mod webdav;

pub use account::Account;

/// Nombre de agente HTTP que envían las apps (cada una añade su nombre y versión).
pub fn user_agent(app: &str, version: &str) -> String {
    format!("{app}/{version} iurefficient-connect/{}", env!("CARGO_PKG_VERSION"))
}

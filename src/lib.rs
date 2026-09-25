//! Conector común de las aplicaciones de escritorio de Iurefficient
//! (IureTranscribe, IureEditor, IureDav, IureOCR) con una instancia.
//!
//! - [`account`]: identidad de la instancia (dominio normalizado) y del usuario, y la
//!   cuenta activa compartida (qué instancia y correo usó la última app que inició sesión).
//! - [`secrets`]: credenciales en el llavero del sistema, compartidas entre apps.
//! - [`webdav`]: árbol de documentos (listar, subir, descargar) con contraseña `iurdav_…`.
//! - [`mcp`]: servidor MCP de sólo lectura con token `iurmcp_…`.
//! - [`rest`]: API REST con sesión de cookies (JWT + CSRF + refresco).
//! - [`api`]: operaciones tipadas: proyectos, documentos, minutas por blueprint, CRM, horas.
//! - [`releases`]: aviso de versiones nuevas publicadas en GitHub.
//! - [`lang`]: idioma de la interfaz (inglés por defecto, español si el sistema lo está).
//! - [`apps`]: catálogo de las apps de escritorio: detección, lanzamiento y última versión.
//!
//! Sin dependencias de Tauri ni de ninguna interfaz: cada app pone su propia UI encima.

pub mod account;
pub mod api;
pub mod apps;
pub mod lang;
pub mod mcp;
pub mod releases;
pub mod rest;
#[cfg(feature = "keyring")]
pub mod secrets;
pub mod webdav;

pub use account::{Account, ActiveAccount};

/// Nombre de agente HTTP que envían las apps (cada una añade su nombre y versión).
pub fn user_agent(app: &str, version: &str) -> String {
    format!(
        "{app}/{version} iurefficient-connect/{}",
        env!("CARGO_PKG_VERSION")
    )
}

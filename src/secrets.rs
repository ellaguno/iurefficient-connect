//! Credenciales en el llavero del sistema (Secret Service en Linux, Llaveros en macOS,
//! Administrador de credenciales en Windows). Nunca en ficheros de configuración.
//!
//! Las entradas se comparten entre las apps: si IureDav guardó la contraseña WebDAV
//! de una instancia, IureTranscribe la encuentra sin volver a pedirla.

use anyhow::{Context, Result};
use keyring::Entry;

use crate::{lang, Account};

const SERVICIO: &str = "iurefficient";

/// Tipo de credencial guardada para una cuenta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Contraseña de aplicación WebDAV (`iurdav_…`).
    WebDav,
    /// Token del servidor MCP (`iurmcp_…`).
    Mcp,
    /// Sesión REST serializada (cookies de refresco).
    Session,
}

impl Kind {
    fn sufijo(self) -> &'static str {
        match self {
            Kind::WebDav => "webdav",
            Kind::Mcp => "mcp",
            Kind::Session => "session",
        }
    }
}

fn entrada(acc: &Account, kind: Kind) -> Result<Entry> {
    Entry::new(SERVICIO, &format!("{}:{}:{}", acc.host(), acc.email, kind.sufijo())).with_context(|| lang::pick("could not open the system keyring", "no se pudo abrir el llavero del sistema"))
}

pub fn guardar(acc: &Account, kind: Kind, secreto: &str) -> Result<()> {
    entrada(acc, kind)?.set_password(secreto).with_context(|| lang::pick("could not save to the keyring", "no se pudo guardar en el llavero"))
}

/// `Ok(None)` si no hay nada guardado, distinto de que el llavero falle.
pub fn leer(acc: &Account, kind: Kind) -> Result<Option<String>> {
    match entrada(acc, kind)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e).with_context(|| lang::pick("could not read from the keyring", "no se pudo leer del llavero")),
    }
}

pub fn borrar(acc: &Account, kind: Kind) -> Result<()> {
    match entrada(acc, kind)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e).with_context(|| lang::pick("could not delete from the keyring", "no se pudo borrar del llavero")),
    }
}

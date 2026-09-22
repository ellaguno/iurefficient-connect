//! Identidad de la instancia y del usuario.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use url::Url;

/// Una instancia de Iurefficient y el usuario que la usa. Las credenciales
/// (contraseña WebDAV, token MCP, sesión REST) viven aparte, en [`crate::secrets`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Raíz de la instancia, siempre con barra final, sin ruta ni query: `https://2.ds.iurefficient.com/`.
    pub base: Url,
    pub email: String,
}

impl Account {
    /// Acepta `2.ds.iurefficient.com`, `https://2.ds.iurefficient.com/` o una URL con ruta
    /// (`…/webdav/`), y la reduce a la raíz de la instancia.
    pub fn new(domain: &str, email: &str) -> Result<Self> {
        let d = domain.trim().trim_end_matches('/');
        if d.is_empty() {
            return Err(anyhow!(
                "Indica el dominio de tu instancia (p. ej. 2.ds.iurefficient.com)"
            ));
        }
        let with_scheme = if d.contains("://") {
            d.to_string()
        } else {
            format!("https://{d}")
        };
        let mut base = Url::parse(&with_scheme).map_err(|e| anyhow!("Dominio no válido: {e}"))?;
        if base.host_str().is_none() {
            return Err(anyhow!("Dominio no válido"));
        }
        base.set_path("/");
        base.set_query(None);
        base.set_fragment(None);
        let email = email.trim().to_string();
        if email.is_empty() {
            return Err(anyhow!("Falta el correo del usuario"));
        }
        Ok(Self { base, email })
    }

    /// `host[:puerto]`, útil como identificador estable (llavero, perfiles).
    pub fn host(&self) -> String {
        match self.base.port() {
            Some(p) => format!("{}:{p}", self.base.host_str().unwrap_or_default()),
            None => self.base.host_str().unwrap_or_default().to_string(),
        }
    }

    /// Página principal de la instancia, para «Abrir en Iurefficient».
    pub fn web_url(&self) -> String {
        self.base.to_string()
    }

    /// URL absoluta de una ruta de la API (`/api/...`).
    pub fn api(&self, path: &str) -> Result<Url> {
        Ok(self.base.join(path.trim_start_matches('/'))?)
    }
}

// ---------------------------------------------------------------------------
// Cuenta activa compartida entre las apps
// ---------------------------------------------------------------------------

/// Qué instancia y qué correo usó la última app que inició sesión en este equipo.
///
/// Es lo que permite que, tras conectar IureDav, IureTranscribe e IureEditor
/// arranquen ya conectados: con esto saben qué entrada del llavero buscar. No es
/// un secreto (las credenciales van al llavero), por eso vive en un archivo
/// legible: `<config>/iurefficient/cuenta-activa.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveAccount {
    /// `host[:puerto]` de la instancia, como lo devuelve [`Account::host`].
    pub domain: String,
    pub email: String,
    /// App que la escribió (`IureDav`, `IureTranscribe`, `IureEditor`).
    #[serde(default)]
    pub app: String,
    /// Segundos desde la época Unix.
    #[serde(default)]
    pub updated_at: u64,
}

impl ActiveAccount {
    pub fn account(&self) -> Result<Account> {
        Account::new(&self.domain, &self.email)
    }
}

/// Carpeta de configuración común a las tres apps (`~/.config/iurefficient`,
/// `~/Library/Application Support/iurefficient`, `%APPDATA%\iurefficient`).
pub fn shared_config_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("iurefficient"))
}

fn active_path() -> Option<PathBuf> {
    shared_config_dir().map(|d| d.join("cuenta-activa.json"))
}

/// La cuenta activa, si alguna app la dejó escrita y sigue siendo válida.
pub fn active() -> Option<ActiveAccount> {
    let text = std::fs::read_to_string(active_path()?).ok()?;
    let a: ActiveAccount = serde_json::from_str(&text).ok()?;
    a.account().ok()?;
    Some(a)
}

/// Deja `acc` como cuenta activa para las demás apps. Se llama al iniciar sesión.
pub fn set_active(acc: &Account, app: &str) -> Result<()> {
    let path = active_path().ok_or_else(|| anyhow!("no hay carpeta de configuración"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let a = ActiveAccount {
        domain: acc.host(),
        email: acc.email.clone(),
        app: app.to_string(),
        updated_at,
    };
    std::fs::write(&path, serde_json::to_string_pretty(&a)?)?;
    Ok(())
}

/// Olvida la cuenta activa si es `acc`. Se llama al cerrar sesión: las otras apps
/// dejan de restaurarla sola.
pub fn clear_active(acc: &Account) -> Result<()> {
    let Some(path) = active_path() else {
        return Ok(());
    };
    if let Some(a) = active() {
        if a.domain == acc.host() && a.email == acc.email {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuenta_activa_serializa_y_valida() {
        let a = ActiveAccount {
            domain: "2.ds.iurefficient.com".into(),
            email: "a@b.c".into(),
            app: "IureDav".into(),
            updated_at: 1,
        };
        let json = serde_json::to_string(&a).unwrap();
        let b: ActiveAccount = serde_json::from_str(&json).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            b.account().unwrap().web_url(),
            "https://2.ds.iurefficient.com/"
        );
        // Campos nuevos opcionales: un archivo antiguo sin `app` sigue leyéndose.
        let c: ActiveAccount =
            serde_json::from_str(r#"{"domain":"x.com","email":"a@b.c"}"#).unwrap();
        assert_eq!(c.app, "");
        assert!(ActiveAccount {
            domain: String::new(),
            email: "a@b.c".into(),
            app: String::new(),
            updated_at: 0
        }
        .account()
        .is_err());
    }

    #[test]
    fn normaliza_dominio() {
        let a = Account::new("2.ds.iurefficient.com", "a@b.c").unwrap();
        assert_eq!(a.web_url(), "https://2.ds.iurefficient.com/");
        assert_eq!(a.host(), "2.ds.iurefficient.com");
        let a = Account::new("https://demo.iurefficient.com/webdav/", " a@b.c ").unwrap();
        assert_eq!(a.web_url(), "https://demo.iurefficient.com/");
        assert_eq!(a.email, "a@b.c");
        let a = Account::new("http://127.0.0.1:8099", "a@b.c").unwrap();
        assert_eq!(a.host(), "127.0.0.1:8099");
        assert_eq!(
            a.api("/api/auth/me").unwrap().as_str(),
            "http://127.0.0.1:8099/api/auth/me"
        );
        assert!(Account::new("", "a@b.c").is_err());
        assert!(Account::new("x.com", "").is_err());
    }
}

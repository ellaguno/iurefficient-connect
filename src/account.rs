//! Identidad de la instancia y del usuario.

use anyhow::{anyhow, Result};
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
            return Err(anyhow!("Indica el dominio de tu instancia (p. ej. 2.ds.iurefficient.com)"));
        }
        let with_scheme = if d.contains("://") { d.to_string() } else { format!("https://{d}") };
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(a.api("/api/auth/me").unwrap().as_str(), "http://127.0.0.1:8099/api/auth/me");
        assert!(Account::new("", "a@b.c").is_err());
        assert!(Account::new("x.com", "").is_err());
    }
}

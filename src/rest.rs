//! API REST de la instancia con sesión de cookies: la instancia guarda el JWT en
//! cookies httpOnly (`access_token_cookie`, 1 h) y el refresco en
//! `refresh_token_cookie` (30 días), con CSRF de doble envío en POST/PUT/DELETE
//! (cookie `csrf_access_token` → cabecera `X-CSRF-TOKEN`).
//!
//! La sesión se puede serializar ([`Session::export`]) para guardarla en el llavero
//! y restaurarla al arrancar ([`Session::import`]); el token de acceso se renueva solo.

use anyhow::{anyhow, Context, Result};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::Account;

pub const CSRF_HEADER: &str = "X-CSRF-TOKEN";
const CSRF_ACCESS_COOKIE: &str = "csrf_access_token";
const CSRF_REFRESH_COOKIE: &str = "csrf_refresh_token";
const REFRESH_COOKIE: &str = "refresh_token_cookie";
/// Antes de que caduque el acceso (1 h) se renueva de forma preventiva.
const REFRESH_EVERY: Duration = Duration::from_secs(50 * 60);

/// Resultado de `login`: sesión abierta, o falta el código TOTP.
#[derive(Debug)]
pub enum Login {
    Ok(User),
    /// La cuenta tiene verificación en dos pasos: llama a [`Session::verify_totp`].
    TotpRequired { totp_token: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct User {
    #[serde(default)]
    pub id: Value,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// Cookies serializadas para guardar en el llavero y restaurar la sesión.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionExport {
    pub cookies: Vec<String>,
}

pub struct Session {
    acc: Account,
    jar: Arc<Jar>,
    http: Client,
    last_refresh: Mutex<Option<Instant>>,
}

impl Session {
    pub fn new(acc: Account, user_agent: &str) -> Result<Self> {
        let jar = Arc::new(Jar::default());
        let http = Client::builder()
            .user_agent(user_agent)
            .cookie_provider(jar.clone())
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(20))
            .build()?;
        Ok(Self { acc, jar, http, last_refresh: Mutex::new(None) })
    }

    pub fn account(&self) -> &Account {
        &self.acc
    }

    fn cookie(&self, name: &str) -> Option<String> {
        let hv = self.jar.cookies(&self.acc.base)?;
        let s = hv.to_str().ok()?;
        s.split(';').map(str::trim).find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_string))
    }

    /// `true` si hay cookie de refresco (la sesión puede intentar renovarse).
    pub fn has_refresh(&self) -> bool {
        self.cookie(REFRESH_COOKIE).is_some()
    }

    /// Cookies de la sesión, para guardarlas en el llavero.
    pub fn export(&self) -> SessionExport {
        let cookies = self
            .jar
            .cookies(&self.acc.base)
            .and_then(|hv| hv.to_str().ok().map(str::to_string))
            .map(|s| s.split(';').map(|kv| kv.trim().to_string()).filter(|kv| !kv.is_empty()).collect())
            .unwrap_or_default();
        SessionExport { cookies }
    }

    /// Restaura cookies guardadas y renueva el acceso.
    pub async fn import(&self, saved: &SessionExport) -> Result<User> {
        for kv in &saved.cookies {
            // Path amplio para que reqwest las envíe a /api/…; el servidor sólo lee las suyas.
            self.jar.add_cookie_str(&format!("{kv}; Path=/; Secure; HttpOnly"), &self.acc.base);
        }
        self.refresh().await?;
        self.me().await
    }

    async fn send(&self, req: RequestBuilder) -> Result<reqwest::Response> {
        let resp = req.send().await.with_context(|| format!("No se pudo conectar con {}", self.acc.base))?;
        Ok(resp)
    }

    /// Petición con CSRF y renovación automática del acceso.
    pub async fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        self.ensure_fresh().await?;
        let url = self.acc.api(path)?;
        let mut rb = self.http.request(method.clone(), url);
        if matches!(method, Method::POST | Method::PUT | Method::PATCH | Method::DELETE) {
            if let Some(csrf) = self.cookie(CSRF_ACCESS_COOKIE) {
                rb = rb.header(CSRF_HEADER, csrf);
            }
        }
        Ok(rb)
    }

    async fn ensure_fresh(&self) -> Result<()> {
        let due = {
            let last = self.last_refresh.lock().await;
            match *last {
                Some(t) => t.elapsed() > REFRESH_EVERY,
                None => false,
            }
        };
        if due && self.has_refresh() {
            self.refresh().await?;
        }
        Ok(())
    }

    /// `POST /api/auth/login`. Devuelve la sesión abierta o la petición de TOTP.
    pub async fn login(&self, password: &str) -> Result<Login> {
        let url = self.acc.api("/api/auth/login")?;
        let resp = self
            .send(self.http.post(url).json(&serde_json::json!({"email": self.acc.email, "password": password})))
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        match status {
            StatusCode::OK => {
                if body.get("requires_totp").and_then(|b| b.as_bool()).unwrap_or(false) {
                    let t = body.get("totp_token").and_then(|t| t.as_str()).unwrap_or_default().to_string();
                    return Ok(Login::TotpRequired { totp_token: t });
                }
                *self.last_refresh.lock().await = Some(Instant::now());
                Ok(Login::Ok(user_from(&body).unwrap_or_default()))
            }
            StatusCode::UNAUTHORIZED => Err(anyhow!("Correo o contraseña incorrectos")),
            StatusCode::FORBIDDEN => Err(anyhow!("La cuenta está desactivada")),
            StatusCode::TOO_MANY_REQUESTS => Err(anyhow!("Demasiados intentos; espera unos minutos")),
            s => Err(anyhow!("El servidor respondió {s}: {}", body.get("error").and_then(|e| e.as_str()).unwrap_or(""))),
        }
    }

    /// Segundo paso cuando `login` devolvió [`Login::TotpRequired`].
    pub async fn verify_totp(&self, totp_token: &str, code: &str) -> Result<User> {
        let url = self.acc.api("/api/auth/verify-totp")?;
        let resp = self
            .send(self.http.post(url).bearer_auth(totp_token).json(&serde_json::json!({"totp_token": totp_token, "code": code.trim(), "token": code.trim()})))
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!("Código no válido ({status})"));
        }
        *self.last_refresh.lock().await = Some(Instant::now());
        Ok(user_from(&body).unwrap_or_default())
    }

    /// `POST /api/auth/refresh` con la cookie de refresco.
    pub async fn refresh(&self) -> Result<()> {
        let url = self.acc.api("/api/auth/refresh")?;
        let mut rb = self.http.post(url);
        if let Some(csrf) = self.cookie(CSRF_REFRESH_COOKIE) {
            rb = rb.header(CSRF_HEADER, csrf);
        }
        let resp = self.send(rb).await?;
        match resp.status() {
            s if s.is_success() => {
                *self.last_refresh.lock().await = Some(Instant::now());
                Ok(())
            }
            StatusCode::UNAUTHORIZED => Err(anyhow!("La sesión caducó; vuelve a iniciar sesión")),
            s => Err(anyhow!("No se pudo renovar la sesión ({s})")),
        }
    }

    /// `GET /api/auth/me`.
    pub async fn me(&self) -> Result<User> {
        let resp = self.send(self.request(Method::GET, "/api/auth/me").await?).await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(anyhow!("Sesión no válida; inicia sesión de nuevo"));
        }
        let body: Value = resp.error_for_status()?.json().await?;
        Ok(user_from(&body).unwrap_or_default())
    }

    /// GET que devuelve JSON.
    pub async fn get_json(&self, path: &str) -> Result<Value> {
        let resp = self.send(self.request(Method::GET, path).await?).await?;
        json_or_error(resp).await
    }

    /// POST con cuerpo JSON.
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let resp = self.send(self.request(Method::POST, path).await?.json(body)).await?;
        json_or_error(resp).await
    }

    /// POST multipart (subida de archivos).
    pub async fn post_multipart(&self, path: &str, form: reqwest::multipart::Form) -> Result<Value> {
        let resp = self.send(self.request(Method::POST, path).await?.multipart(form)).await?;
        json_or_error(resp).await
    }
}

fn user_from(body: &Value) -> Option<User> {
    let u = body.get("user").cloned().unwrap_or_else(|| body.clone());
    serde_json::from_value(u).ok()
}

async fn json_or_error(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    let msg = body
        .get("error")
        .or_else(|| body.get("message"))
        .and_then(|e| e.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| text.chars().take(200).collect());
    Err(anyhow!("{status}: {msg}"))
}

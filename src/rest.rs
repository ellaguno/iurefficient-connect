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
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

use crate::{lang, tr, Account};

pub const CSRF_HEADER: &str = "X-CSRF-TOKEN";
const CSRF_ACCESS_COOKIE: &str = "csrf_access_token";
const CSRF_REFRESH_COOKIE: &str = "csrf_refresh_token";
const REFRESH_COOKIE: &str = "refresh_token_cookie";
const ACCESS_COOKIE: &str = "access_token_cookie";
/// Antes de que caduque el acceso (1 h) se renueva de forma preventiva.
const REFRESH_EVERY: Duration = Duration::from_secs(50 * 60);

/// Resultado de `login`: sesión abierta, o falta el código TOTP.
#[derive(Debug)]
pub enum Login {
    Ok(User),
    /// La cuenta tiene verificación en dos pasos: llama a [`Session::verify_totp`].
    TotpRequired {
        totp_token: String,
    },
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
    /// Hora de reloj (no `Instant`): tras suspender el equipo el acceso ya caducó
    /// aunque el reloj monótono no haya avanzado.
    last_refresh: Mutex<Option<SystemTime>>,
    /// Sincronizar las cookies con la entrada compartida del llavero (ver [`Session::set_shared`]).
    shared: std::sync::atomic::AtomicBool,
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
        Ok(Self {
            acc,
            jar,
            http,
            last_refresh: Mutex::new(None),
            shared: std::sync::atomic::AtomicBool::new(cfg!(feature = "keyring")),
        })
    }

    /// La sesión se comparte con las otras apps a través del llavero (activado por
    /// defecto con la función `keyring`): antes de renovar el acceso se releen las
    /// cookies guardadas, por si otra app ya renovó y el servidor rotó la cookie de
    /// refresco, y tras iniciar sesión o renovar se guardan las nuevas. Sin esto,
    /// dos apps abiertas a la vez con la misma sesión podrían invalidarse entre sí.
    pub fn set_shared(&self, on: bool) {
        self.shared.store(on, std::sync::atomic::Ordering::Relaxed);
    }

    fn is_shared(&self) -> bool {
        cfg!(feature = "keyring") && self.shared.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Relee del llavero las cookies guardadas por cualquier app y las pone en el jar.
    #[cfg(feature = "keyring")]
    fn pull_shared(&self) {
        if !self.is_shared() {
            return;
        }
        if let Ok(Some(json)) = crate::secrets::leer(&self.acc, crate::secrets::Kind::Session) {
            if let Ok(saved) = serde_json::from_str::<SessionExport>(&json) {
                self.add_cookies(&saved);
            }
        }
    }

    /// Guarda las cookies actuales en el llavero para las demás apps.
    #[cfg(feature = "keyring")]
    pub fn push_shared(&self) {
        if !self.is_shared() {
            return;
        }
        if let Ok(json) = serde_json::to_string(&self.export()) {
            if let Err(e) = crate::secrets::guardar(&self.acc, crate::secrets::Kind::Session, &json)
            {
                log::warn!("no se pudo guardar la sesión compartida: {e:#}");
            }
        }
    }

    #[cfg(not(feature = "keyring"))]
    fn pull_shared(&self) {}
    #[cfg(not(feature = "keyring"))]
    pub fn push_shared(&self) {}

    /// La instancia pone cada cookie en su ruta: el acceso en `/api/`, el refresco
    /// sólo en `/api/auth/refresh` y las CSRF en la raíz. Al restaurarlas hay que
    /// respetarlas: con otra ruta convivirían con las del servidor y se mandaría
    /// la vieja.
    fn cookie_path(&self, name: &str) -> String {
        let path = match name {
            ACCESS_COOKIE => "/api/",
            REFRESH_COOKIE => "/api/auth/refresh",
            _ => "/",
        };
        self.acc.api(path).map(|u| u.path().to_string()).unwrap_or_else(|_| path.to_string())
    }

    /// URL a la que el navegador mandaría todas las cookies de la sesión.
    fn all_cookies_url(&self) -> Option<reqwest::Url> {
        self.acc.api("/api/auth/refresh").ok()
    }

    fn add_cookies(&self, saved: &SessionExport) {
        for kv in &saved.cookies {
            let name = kv.split('=').next().unwrap_or_default().trim();
            if name.is_empty() {
                continue;
            }
            let path = self.cookie_path(name);
            self.jar
                .add_cookie_str(&format!("{kv}; Path={path}; Secure; HttpOnly"), &self.acc.base);
        }
    }

    /// Sesión restaurada del llavero compartido: `Ok(None)` si ninguna app inició
    /// sesión con esta cuenta; `Err` si había sesión pero ya no vale.
    #[cfg(feature = "keyring")]
    pub async fn restore_shared(acc: Account, user_agent: &str) -> Result<Option<(Self, User)>> {
        let Some(json) = crate::secrets::leer(&acc, crate::secrets::Kind::Session)? else {
            return Ok(None);
        };
        let Ok(saved) = serde_json::from_str::<SessionExport>(&json) else {
            return Ok(None);
        };
        let sess = Self::new(acc, user_agent)?;
        let user = sess.import(&saved).await?;
        Ok(Some((sess, user)))
    }

    pub fn account(&self) -> &Account {
        &self.acc
    }

    fn cookie(&self, name: &str) -> Option<String> {
        // Desde la raíz sólo se verían las CSRF: el refresco vive en /api/auth/refresh.
        let hv = self.jar.cookies(&self.all_cookies_url()?)?;
        let s = hv.to_str().ok()?;
        s.split(';')
            .map(str::trim)
            .find_map(|kv| kv.strip_prefix(&format!("{name}=")).map(str::to_string))
    }

    /// `true` si hay cookie de refresco (la sesión puede intentar renovarse).
    pub fn has_refresh(&self) -> bool {
        self.cookie(REFRESH_COOKIE).is_some()
    }

    /// Cookies de la sesión, para guardarlas en el llavero.
    pub fn export(&self) -> SessionExport {
        let cookies = self
            .all_cookies_url()
            .and_then(|u| self.jar.cookies(&u))
            .and_then(|hv| hv.to_str().ok().map(str::to_string))
            .map(|s| {
                s.split(';')
                    .map(|kv| kv.trim().to_string())
                    .filter(|kv| !kv.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        SessionExport { cookies }
    }

    /// Restaura cookies guardadas y renueva el acceso.
    pub async fn import(&self, saved: &SessionExport) -> Result<User> {
        self.add_cookies(saved);
        self.refresh().await?;
        self.me().await
    }

    async fn send(&self, req: RequestBuilder) -> Result<reqwest::Response> {
        let resp = req
            .send()
            .await
            .with_context(|| tr!("Could not connect to {}", "No se pudo conectar con {}", self.acc.base))?;
        Ok(resp)
    }

    /// Petición con CSRF y renovación automática del acceso.
    pub async fn request(&self, method: Method, path: &str) -> Result<RequestBuilder> {
        self.ensure_fresh().await?;
        let url = self.acc.api(path)?;
        let mut rb = self.http.request(method.clone(), url);
        if matches!(
            method,
            Method::POST | Method::PUT | Method::PATCH | Method::DELETE
        ) {
            if let Some(csrf) = self.cookie(CSRF_ACCESS_COOKIE) {
                rb = rb.header(CSRF_HEADER, csrf);
            }
        }
        Ok(rb)
    }

    /// Envía y, si el acceso ya no vale (401: caducó, se suspendió el equipo u otra
    /// app renovó), renueva una vez y repite.
    async fn send_retrying(&self, method: Method, path: &str, body: Option<&Value>) -> Result<reqwest::Response> {
        let build = |rb: RequestBuilder| match body {
            Some(b) => rb.json(b),
            None => rb,
        };
        let resp = self.send(build(self.request(method.clone(), path).await?)).await?;
        if resp.status() == StatusCode::UNAUTHORIZED && self.has_refresh() && self.refresh().await.is_ok() {
            return self.send(build(self.request(method, path).await?)).await;
        }
        Ok(resp)
    }

    async fn ensure_fresh(&self) -> Result<()> {
        let due = {
            let last = self.last_refresh.lock().await;
            match *last {
                Some(t) => t.elapsed().map_or(true, |e| e > REFRESH_EVERY),
                None => false,
            }
        };
        if due && self.has_refresh() {
            self.refresh().await?;
        }
        Ok(())
    }

    /// `POST /api/auth/login`. Devuelve la sesión abierta o la petición de TOTP.
    /// `POST /api/auth/logout` (invalida el acceso y borra las cookies).
    pub async fn logout(&self) -> Result<()> {
        let _ = self
            .send(self.request(Method::POST, "/api/auth/logout").await?)
            .await;
        Ok(())
    }

    pub async fn login(&self, password: &str) -> Result<Login> {
        let url = self.acc.api("/api/auth/login")?;
        let resp = self
            .send(
                self.http
                    .post(url)
                    .json(&serde_json::json!({"email": self.acc.email, "password": password})),
            )
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        match status {
            StatusCode::OK => {
                if body
                    .get("requires_totp")
                    .and_then(|b| b.as_bool())
                    .unwrap_or(false)
                {
                    let t = body
                        .get("totp_token")
                        .and_then(|t| t.as_str())
                        .unwrap_or_default()
                        .to_string();
                    return Ok(Login::TotpRequired { totp_token: t });
                }
                *self.last_refresh.lock().await = Some(SystemTime::now());
                self.push_shared();
                Ok(Login::Ok(user_from(&body).unwrap_or_default()))
            }
            StatusCode::UNAUTHORIZED => Err(anyhow!(lang::pick("Incorrect email or password", "Correo o contraseña incorrectos"))),
            StatusCode::FORBIDDEN => Err(anyhow!(lang::pick("The account is disabled", "La cuenta está desactivada"))),
            StatusCode::TOO_MANY_REQUESTS => {
                Err(anyhow!(lang::pick("Too many attempts; wait a few minutes", "Demasiados intentos; espera unos minutos")))
            }
            s => Err(anyhow!(tr!(
                "The server responded {s}: {}",
                "El servidor respondió {s}: {}",
                body.get("error").and_then(|e| e.as_str()).unwrap_or("")
            ))),
        }
    }

    /// Segundo paso cuando `login` devolvió [`Login::TotpRequired`].
    pub async fn verify_totp(&self, totp_token: &str, code: &str) -> Result<User> {
        let url = self.acc.api("/api/auth/verify-totp")?;
        let resp = self
            .send(
                self.http
                    .post(url)
                    .json(&serde_json::json!({"totp_token": totp_token, "code": code.trim()})),
            )
            .await?;
        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(anyhow!(tr!("Invalid code ({status})", "Código no válido ({status})")));
        }
        *self.last_refresh.lock().await = Some(SystemTime::now());
        self.push_shared();
        Ok(user_from(&body).unwrap_or_default())
    }

    /// `POST /api/auth/refresh` con la cookie de refresco.
    pub async fn refresh(&self) -> Result<()> {
        // Otra app pudo renovar antes: si el servidor rota la cookie de refresco, la
        // buena es la del llavero, no la que tenemos en memoria.
        self.pull_shared();
        let url = self.acc.api("/api/auth/refresh")?;
        let mut rb = self.http.post(url);
        if let Some(csrf) = self.cookie(CSRF_REFRESH_COOKIE) {
            rb = rb.header(CSRF_HEADER, csrf);
        }
        let resp = self.send(rb).await?;
        match resp.status() {
            s if s.is_success() => {
                *self.last_refresh.lock().await = Some(SystemTime::now());
                self.push_shared();
                Ok(())
            }
            StatusCode::UNAUTHORIZED => Err(anyhow!(lang::pick("The session expired; sign in again", "La sesión caducó; vuelve a iniciar sesión"))),
            s => Err(anyhow!(tr!("Could not renew the session ({s})", "No se pudo renovar la sesión ({s})"))),
        }
    }

    /// `GET /api/auth/me`.
    pub async fn me(&self) -> Result<User> {
        let resp = self.send_retrying(Method::GET, "/api/auth/me", None).await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            return Err(anyhow!(lang::pick("Invalid session; sign in again", "Sesión no válida; inicia sesión de nuevo")));
        }
        let body: Value = resp.error_for_status()?.json().await?;
        Ok(user_from(&body).unwrap_or_default())
    }

    /// GET que devuelve JSON.
    pub async fn get_json(&self, path: &str) -> Result<Value> {
        let resp = self.send_retrying(Method::GET, path, None).await?;
        json_or_error(resp).await
    }

    /// POST con cuerpo JSON.
    pub async fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let resp = self.send_retrying(Method::POST, path, Some(body)).await?;
        json_or_error(resp).await
    }

    /// POST multipart (subida de archivos).
    pub async fn post_multipart(
        &self,
        path: &str,
        form: reqwest::multipart::Form,
    ) -> Result<Value> {
        let resp = self
            .send(self.request(Method::POST, path).await?.multipart(form))
            .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session::new(Account::new("x.example.com", "a@b.c").unwrap(), "test").unwrap()
    }

    /// Las cookies como las pone la instancia (cada una en su ruta).
    fn server_cookies(s: &Session, suffix: &str) {
        for c in [
            format!("access_token_cookie=A{suffix}; Path=/api/; Secure; HttpOnly"),
            format!("refresh_token_cookie=R{suffix}; Path=/api/auth/refresh; Secure; HttpOnly"),
            format!("csrf_access_token=CA{suffix}; Path=/; Secure"),
            format!("csrf_refresh_token=CR{suffix}; Path=/; Secure"),
        ] {
            s.jar.add_cookie_str(&c, &s.acc.base);
        }
    }

    #[test]
    fn export_incluye_acceso_y_refresco() {
        let s = session();
        server_cookies(&s, "1");
        assert!(s.has_refresh());
        let mut names: Vec<String> = s.export().cookies.iter().map(|c| c.split('=').next().unwrap().to_string()).collect();
        names.sort();
        assert_eq!(names, ["access_token_cookie", "csrf_access_token", "csrf_refresh_token", "refresh_token_cookie"]);
    }

    #[test]
    fn import_respeta_rutas_y_reemplaza_las_viejas() {
        let a = session();
        server_cookies(&a, "1");
        let saved = a.export();
        // Otra app con cookies viejas en memoria recibe las nuevas del llavero.
        let b = session();
        server_cookies(&b, "0");
        b.add_cookies(&saved);
        let refresh = b.jar.cookies(&b.acc.api("/api/auth/refresh").unwrap()).unwrap();
        let refresh = refresh.to_str().unwrap();
        assert!(refresh.contains("refresh_token_cookie=R1") && !refresh.contains("R0"), "{refresh}");
        assert!(refresh.contains("csrf_refresh_token=CR1") && !refresh.contains("CR0"), "{refresh}");
        let api = b.jar.cookies(&b.acc.api("/api/cases").unwrap()).unwrap();
        let api = api.to_str().unwrap();
        assert!(api.contains("access_token_cookie=A1") && !api.contains("refresh_token_cookie"), "{api}");
        assert_eq!(b.cookie(CSRF_ACCESS_COOKIE).as_deref(), Some("CA1"));
    }
}

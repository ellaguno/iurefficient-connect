//! Árbol de documentos por WebDAV con contraseña de aplicación (`iurdav_…`): la
//! única credencial de larga vida con escritura que ofrece hoy la instancia.
//!
//! Reglas del servidor que este módulo respeta: `PUT` a ruta nueva crea documento
//! (201), `PUT` a ruta existente crea versión (204); `DELETE`/`MOVE`/`COPY`/`MKCOL`
//! devuelven 405 por diseño; `Vistas (solo navegar)/` no admite escrituras; las
//! extensiones de subtítulos no están entre los tipos permitidos por defecto.

use anyhow::{anyhow, Context, Result};
use percent_encoding::percent_decode_str;
use reqwest::{Client, Method, StatusCode};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;
use url::Url;

use crate::Account;

/// Carpeta de sólo navegación que expone el servidor; no admite escrituras.
const CARPETA_VISTAS: &str = "Vistas (solo navegar)";

/// Acceso WebDAV a una cuenta.
#[derive(Debug, Clone)]
pub struct WebDav {
    pub acc: Account,
    pub password: String,
    pub user_agent: String,
}

impl WebDav {
    pub fn new(acc: Account, password: &str, user_agent: &str) -> Result<Self> {
        if password.trim().is_empty() {
            return Err(anyhow!("Falta la contraseña de aplicación (iurdav_…)"));
        }
        Ok(Self { acc, password: password.trim().to_string(), user_agent: user_agent.to_string() })
    }

    fn webdav_url(&self, folder: &str) -> Result<Url> {
        let mut u = self.acc.base.join("webdav/")?;
        {
            let mut segs = u.path_segments_mut().map_err(|_| anyhow!("URL base no válida"))?;
            segs.pop_if_empty();
            for part in folder.split('/').filter(|p| !p.is_empty()) {
                segs.push(part);
            }
            segs.push(""); // barra final: es una colección
        }
        Ok(u)
    }

    fn file_url(&self, folder: &str, file_name: &str) -> Result<Url> {
        let mut u = self.acc.base.join("webdav/")?;
        {
            let mut segs = u.path_segments_mut().map_err(|_| anyhow!("URL base no válida"))?;
            segs.pop_if_empty();
            for part in folder.split('/').filter(|p| !p.is_empty()) {
                segs.push(part);
            }
            segs.push(file_name);
        }
        Ok(u)
    }

    fn http(&self) -> Result<Client> {
        Client::builder()
            .user_agent(&self.user_agent)
            .timeout(Duration::from_secs(600))
            .connect_timeout(Duration::from_secs(20))
            .build()
            .map_err(|e| e.into())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    /// Ruta relativa a la raíz WebDAV, sin barras al inicio ni al final.
    pub path: String,
    pub is_folder: bool,
    /// Se puede crear documentos aquí (permiso «C» de ownCloud, o desconocido → true salvo en Vistas).
    pub can_upload: bool,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub path: String,
    pub can_upload: bool,
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionInfo {
    pub web_url: String,
    pub root_folders: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Uploaded {
    pub file_name: String,
    pub remote_path: String,
    /// true = documento nuevo (201), false = versión nueva de uno existente (204).
    pub created: bool,
    /// El servidor rechazó la extensión original y se subió con este otro nombre.
    pub renamed_from: Option<String>,
}

/// Extensiones de subtítulos que las instancias suelen no admitir; se reintentan como `.txt`.
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "vtt", "json"];

/// Nombre alternativo cuando la instancia rechaza la extensión: `x.srt` → `x.srt.txt`.
pub fn fallback_name(name: &str) -> Option<String> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    SUBTITLE_EXTENSIONS.contains(&ext.as_str()).then(|| format!("{name}.txt"))
}

fn explain_status(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED => "Credenciales rechazadas. Revisa el correo y la contraseña de aplicación (iurdav_…).".into(),
        StatusCode::FORBIDDEN => "Sin permiso para esa carpeta o usuario desactivado.".into(),
        StatusCode::NOT_FOUND => "El servidor no expone /webdav. Un administrador debe activar WebDAV en la instancia (Ajustes → WebDAV) y reiniciar.".into(),
        StatusCode::METHOD_NOT_ALLOWED => "Operación no permitida por el servidor en esa ruta.".into(),
        StatusCode::PAYLOAD_TOO_LARGE => "El archivo supera el tamaño máximo que acepta la instancia.".into(),
        s => format!("El servidor respondió {s}."),
    }
}

impl WebDav {
/// PROPFIND profundidad 1 sobre una carpeta.
pub async fn list(&self, folder: &str) -> Result<Listing> {
    let client = self.http()?;
    let url = self.webdav_url(folder)?;
    let body = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
  <d:prop><d:displayname/><d:resourcetype/><d:getcontentlength/><oc:permissions/></d:prop>
</d:propfind>"#;
    let resp = client
        .request(Method::from_bytes(b"PROPFIND").unwrap(), url.clone())
        .basic_auth(&self.acc.email, Some(&self.password))
        .header("Depth", "1")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(body)
        .send()
        .await
        .with_context(|| format!("No se pudo conectar con {}", self.acc.base))?;
    let status = resp.status();
    if !(status.is_success() || status == StatusCode::MULTI_STATUS) {
        return Err(anyhow!(explain_status(status)));
    }
    let xml = resp.text().await?;
    parse_multistatus(&xml, folder, url.path())
}
}

fn parse_multistatus(xml: &str, folder: &str, self_path: &str) -> Result<Listing> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| anyhow!("Respuesta WebDAV no válida: {e}"))?;
    let folder_norm = folder.trim_matches('/').to_string();
    let self_norm = percent_decode_str(self_path).decode_utf8_lossy().trim_end_matches('/').to_string();
    let mut entries = Vec::new();
    let mut self_can_upload = !folder_norm.starts_with(CARPETA_VISTAS);
    for response in doc.descendants().filter(|n| n.has_tag_name("response")) {
        let href = response
            .descendants()
            .find(|n| n.has_tag_name("href"))
            .and_then(|n| n.text())
            .unwrap_or("")
            .trim()
            .to_string();
        let decoded = percent_decode_str(&href).decode_utf8_lossy().to_string();
        let path_only = Url::parse(&decoded)
            .map(|u| u.path().to_string())
            .unwrap_or(decoded.clone());
        let clean = path_only.trim_end_matches('/').to_string();
        let is_folder = response
            .descendants()
            .any(|n| n.has_tag_name("resourcetype") && n.children().any(|c| c.has_tag_name("collection")));
        let perms = response
            .descendants()
            .find(|n| n.has_tag_name("permissions"))
            .and_then(|n| n.text())
            .map(|t| t.trim().to_string());
        let can_upload = match perms.as_deref() {
            Some(p) => p.contains('C'),
            None => true,
        };
        let size = response
            .descendants()
            .find(|n| n.has_tag_name("getcontentlength"))
            .and_then(|n| n.text())
            .and_then(|t| t.trim().parse::<u64>().ok());
        if clean == self_norm {
            self_can_upload = can_upload && !folder_norm.starts_with(CARPETA_VISTAS);
            continue;
        }
        let name = match clean.rsplit('/').next() {
            Some(n) if !n.is_empty() => n.to_string(),
            _ => continue,
        };
        let displayname = response
            .descendants()
            .find(|n| n.has_tag_name("displayname"))
            .and_then(|n| n.text())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| name.clone());
        let rel_path = if folder_norm.is_empty() { name.clone() } else { format!("{folder_norm}/{name}") };
        let in_vistas = rel_path.starts_with(CARPETA_VISTAS);
        entries.push(Entry {
            name: displayname,
            path: rel_path,
            is_folder,
            can_upload: is_folder && can_upload && !in_vistas,
            size,
        });
    }
    entries.sort_by(|a, b| b.is_folder.cmp(&a.is_folder).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(Listing { path: folder_norm, can_upload: self_can_upload, entries })
}

impl WebDav {
    /// Comprueba credenciales y devuelve las carpetas de la raíz.
    pub async fn test_connection(&self) -> Result<ConnectionInfo> {
        let root = self.list("").await?;
        Ok(ConnectionInfo {
            web_url: self.acc.web_url(),
            root_folders: root.entries.into_iter().filter(|e| e.is_folder).map(|e| e.name).collect(),
        })
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("srt") => "application/x-subrip",
        Some("vtt") => "text/vtt",
        Some("txt") => "text/plain; charset=utf-8",
        Some("md") | Some("markdown") => "text/markdown; charset=utf-8",
        Some("json") => "application/json",
        Some("wav") => "audio/wav",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("ogg") | Some("oga") | Some("opus") => "audio/ogg",
        Some("flac") => "audio/flac",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

impl WebDav {
/// Sube un archivo local a `folder/<nombre>`. Un nombre existente crea una versión.
/// `progress(bytes_enviados, total)` se llama conforme avanza la subida.
pub async fn upload<F>(&self, folder: &str, local: &Path, remote_name: Option<&str>, mut progress: F) -> Result<Uploaded>
where
    F: FnMut(u64, u64) + Send + 'static,
{
    let name = remote_name
        .map(str::to_string)
        .or_else(|| local.file_name().map(|n| n.to_string_lossy().into_owned()))
        .ok_or_else(|| anyhow!("Nombre de archivo no válido"))?;
    let meta = tokio::fs::metadata(local).await.with_context(|| format!("No se encontró {}", local.display()))?;
    let total = meta.len();
    let file = tokio::fs::File::open(local).await?;
    let mut sent = 0u64;
    let stream = futures_util::StreamExt::map(tokio_util_compat::ReaderStream::new(file), move |chunk| {
        if let Ok(c) = &chunk {
            sent += c.len() as u64;
            progress(sent, total);
        }
        chunk
    });
    let body = reqwest::Body::wrap_stream(stream);
    let url = self.file_url(folder, &name)?;
    let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs());
    let client = self.http()?;
    let mut req = client
        .put(url.clone())
        .basic_auth(&self.acc.email, Some(&self.password))
        .header("Content-Type", content_type_for(local))
        .header("Content-Length", total);
    if let Some(m) = mtime {
        req = req.header("X-OC-Mtime", m);
    }
    let resp = req.body(body).send().await.with_context(|| format!("No se pudo subir {name}"))?;
    let status = resp.status();
    match status {
        StatusCode::CREATED | StatusCode::OK | StatusCode::NO_CONTENT => Ok(Uploaded {
            file_name: name,
            remote_path: percent_decode_str(url.path()).decode_utf8_lossy().trim_start_matches("/webdav/").to_string(),
            created: status == StatusCode::CREATED,
            renamed_from: None,
        }),
        StatusCode::FORBIDDEN => Err(anyhow!(
            "{name}: la instancia rechazó el archivo (403). Suele deberse a que la extensión no está en los tipos permitidos de la instancia, o a falta de permiso en la carpeta."
        )),
        s => Err(anyhow!("{name}: {}", explain_status(s))),
    }
}

/// Sube y, si la instancia rechaza la extensión (403), reintenta con el nombre
/// alternativo de [`fallback_name`]. Devuelve `renamed_from` cuando eso ocurre.
pub async fn upload_with_fallback<F>(&self, folder: &str, local: &Path, progress: F) -> Result<Uploaded>
where
    F: FnMut(u64, u64) + Send + Clone + 'static,
{
    let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match self.upload(folder, local, None, progress.clone()).await {
        Ok(u) => Ok(u),
        Err(e) if e.to_string().contains("403") => match fallback_name(&name) {
            Some(alt) => {
                let mut u = self.upload(folder, local, Some(&alt), progress).await?;
                u.renamed_from = Some(name);
                Ok(u)
            }
            None => Err(e),
        },
        Err(e) => Err(e),
    }
}

/// Descarga un archivo a una ruta local.
pub async fn download(&self, remote_path: &str, local: &Path) -> Result<u64> {
    let (folder, name) = match remote_path.trim_matches('/').rsplit_once('/') {
        Some((f, n)) => (f, n),
        None => ("", remote_path.trim_matches('/')),
    };
    let url = self.file_url(folder, name)?;
    let resp = self.http()?.get(url).basic_auth(&self.acc.email, Some(&self.password)).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("{name}: {}", explain_status(status)));
    }
    let bytes = resp.bytes().await?;
    tokio::fs::write(local, &bytes).await?;
    Ok(bytes.len() as u64)
}
}

/// Lector de archivo como flujo de bytes (evita depender de tokio-util completo).
mod tokio_util_compat {
    use bytes::Bytes;
    use futures_util::Stream;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    pub struct ReaderStream<R> {
        reader: R,
        buf: Vec<u8>,
        done: bool,
    }

    impl<R: AsyncRead + Unpin> ReaderStream<R> {
        pub fn new(reader: R) -> Self {
            Self { reader, buf: vec![0u8; 256 * 1024], done: false }
        }
    }

    impl<R: AsyncRead + Unpin> Stream for ReaderStream<R> {
        type Item = std::io::Result<Bytes>;
        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = self.get_mut();
            if this.done {
                return Poll::Ready(None);
            }
            let mut rb = ReadBuf::new(&mut this.buf);
            match Pin::new(&mut this.reader).poll_read(cx, &mut rb) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(Err(e)) => {
                    this.done = true;
                    Poll::Ready(Some(Err(e)))
                }
                Poll::Ready(Ok(())) => {
                    let n = rb.filled().len();
                    if n == 0 {
                        this.done = true;
                        Poll::Ready(None)
                    } else {
                        Poll::Ready(Some(Ok(Bytes::copy_from_slice(&this.buf[..n]))))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prueba de integración contra un servidor real o el doble de pruebas de iuredav
    /// (`python3 tests/servidor-falso.py 8099`). Requiere IURE_TEST_URL, IURE_TEST_USER,
    /// IURE_TEST_PASS; se omite si no están definidas.
    #[test]
    fn integration_list_and_upload() {
        let (Ok(url), Ok(user), Ok(pass)) = (std::env::var("IURE_TEST_URL"), std::env::var("IURE_TEST_USER"), std::env::var("IURE_TEST_PASS")) else {
            return;
        };
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let acc = Account::new(&url, &user).unwrap();
            let dav = WebDav::new(acc, &pass, "iurefficient-connect-test/0").unwrap();
            let info = dav.test_connection().await.expect("conexión");
            eprintln!("raíz: {:?}", info.root_folders);
            assert!(!info.root_folders.is_empty(), "sin carpetas en la raíz");
            // Busca una carpeta donde se pueda escribir (raíz o un nivel abajo).
            let root = dav.list("").await.unwrap();
            let mut target: Option<String> = None;
            for e in root.entries.iter().filter(|e| e.is_folder) {
                if e.can_upload {
                    target = Some(e.path.clone());
                    break;
                }
                let sub = dav.list(&e.path).await.unwrap();
                if let Some(w) = sub.entries.iter().find(|x| x.is_folder && x.can_upload) {
                    target = Some(w.path.clone());
                    break;
                }
            }
            let target = target.expect("ninguna carpeta escribible");
            eprintln!("carpeta destino: {target}");
            let tmp = std::env::temp_dir().join("iuretranscribe-selftest.md");
            std::fs::write(&tmp, "# Prueba de IureTranscribe\n\nSubida automática de verificación.\n").unwrap();
            let up = dav.upload(&target, &tmp, Some(".iuretranscribe-selftest.md"), |s, t| eprintln!("  {s}/{t}")).await.expect("subida");
            eprintln!("subido: {:?}", up);
            assert!(up.remote_path.ends_with(".iuretranscribe-selftest.md"));
            // Segunda subida con el mismo nombre → versión (204) o creación según el servidor.
            let up2 = dav.upload(&target, &tmp, Some(".iuretranscribe-selftest.md"), |_, _| {}).await.expect("segunda subida");
            eprintln!("segunda: created={}", up2.created);
        });
    }

    #[test]
    fn urls_webdav() {
        let a = WebDav::new(Account::new("https://demo.iurefficient.com/webdav/", "a@b.c").unwrap(), "iurdav_x", "t/0").unwrap();
        assert_eq!(a.webdav_url("Clientes/Acme S.A./Proyecto 1").unwrap().as_str(), "https://demo.iurefficient.com/webdav/Clientes/Acme%20S.A./Proyecto%201/");
        assert_eq!(a.webdav_url("").unwrap().as_str(), "https://demo.iurefficient.com/webdav/");
        assert_eq!(a.file_url("General", "minuta ñ.md").unwrap().as_str(), "https://demo.iurefficient.com/webdav/General/minuta%20%C3%B1.md");
    }

    #[test]
    fn fallback_names() {
        assert_eq!(fallback_name("pavel_final.srt").as_deref(), Some("pavel_final.srt.txt"));
        assert_eq!(fallback_name("a.VTT").as_deref(), Some("a.VTT.txt"));
        assert_eq!(fallback_name("resumen.md"), None);
        assert_eq!(fallback_name("audio.wav"), None);
    }

    #[test]
    fn parses_multistatus() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns">
<d:response><d:href>/webdav/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><oc:permissions>G</oc:permissions></d:prop></d:propstat></d:response>
<d:response><d:href>/webdav/Clientes/</d:href><d:propstat><d:prop><d:displayname>Clientes</d:displayname><d:resourcetype><d:collection/></d:resourcetype><oc:permissions>G</oc:permissions></d:prop></d:propstat></d:response>
<d:response><d:href>/webdav/General/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><oc:permissions>GC</oc:permissions></d:prop></d:propstat></d:response>
<d:response><d:href>/webdav/Vistas%20(solo%20navegar)/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><oc:permissions>G</oc:permissions></d:prop></d:propstat></d:response>
<d:response><d:href>/webdav/nota.md</d:href><d:propstat><d:prop><d:resourcetype/><d:getcontentlength>12</d:getcontentlength></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let l = parse_multistatus(xml, "", "/webdav/").unwrap();
        assert_eq!(l.path, "");
        assert!(!l.can_upload);
        let names: Vec<_> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Clientes", "General", "Vistas (solo navegar)", "nota.md"]);
        assert!(!l.entries[0].can_upload);
        assert!(l.entries[1].can_upload);
        assert!(!l.entries[2].can_upload);
        assert!(!l.entries[3].is_folder);
        assert_eq!(l.entries[3].size, Some(12));
    }
}

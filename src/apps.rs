//! Catálogo de las aplicaciones de escritorio de Iurefficient: detección de las
//! instaladas en este equipo, lanzamiento con argumentos y consulta de la última
//! versión publicada. Sirve para el panel «Apps de Iurefficient» de cada app y para
//! «Abrir con IureEditor», «Transcribir con IureTranscribe», etc.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::releases;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AppId {
    Transcribe,
    Editor,
    Dav,
    Ocr,
}

impl AppId {
    pub fn all() -> [AppId; 4] {
        [AppId::Transcribe, AppId::Editor, AppId::Dav, AppId::Ocr]
    }
    pub fn parse(s: &str) -> Option<AppId> {
        let s = s.trim().to_ascii_lowercase();
        let s = s.strip_prefix("iure").unwrap_or(&s);
        match s {
            "transcribe" => Some(AppId::Transcribe),
            "editor" | "ditor" => Some(AppId::Editor),
            "dav" => Some(AppId::Dav),
            "ocr" => Some(AppId::Ocr),
            _ => None,
        }
    }
}

/// Definición estática de una app: nombres de binario y de producto por sistema.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppDef {
    pub id: AppId,
    pub name: &'static str,
    pub description: &'static str,
    pub repo: &'static str,
    pub scheme: &'static str,
    /// Nombre del ejecutable en Linux (`/usr/bin/<bin>`).
    pub linux_bin: &'static str,
    /// Nombre de producto (carpeta y `.exe` en Windows, `.app` en macOS).
    pub product: &'static str,
    /// Nombre del `.exe` en Windows (sin extensión) si difiere del producto.
    pub windows_exe: &'static str,
}

pub const APPS: &[AppDef] = &[
    AppDef {
        id: AppId::Transcribe,
        name: "IureTranscribe",
        description: "Transcribe audio y video localmente, graba reuniones y genera resúmenes y minutas.",
        repo: "ellaguno/iuretranscribe",
        scheme: "iuretranscribe",
        linux_bin: "iuretranscribe",
        product: "IureTranscribe",
        windows_exe: "IureTranscribe",
    },
    AppDef {
        id: AppId::Editor,
        name: "IureEditor",
        description: "Editor Markdown con diagramas, fórmulas y exportación a PDF y DOCX.",
        repo: "ellaguno/iureditor",
        scheme: "iureditor",
        linux_bin: "iureditor",
        product: "iureditor",
        windows_exe: "iureditor",
    },
    AppDef {
        id: AppId::Dav,
        name: "IureDav",
        description: "Monta los documentos de Iurefficient como una unidad de tu equipo.",
        repo: "ellaguno/iuredav",
        scheme: "iuredav",
        linux_bin: "iuredav-app",
        product: "IureDav",
        windows_exe: "iuredav-app",
    },
    AppDef {
        id: AppId::Ocr,
        name: "IureOCR",
        description: "Reconoce el texto de escaneos y fotos en tu equipo y deja PDF buscables listos para Iurefficient.",
        repo: "ellaguno/iureocr",
        scheme: "iureocr",
        linux_bin: "iureocr",
        product: "IureOCR",
        windows_exe: "iureocr",
    },
];

pub fn def(id: AppId) -> &'static AppDef {
    APPS.iter().find(|a| a.id == id).expect("catálogo completo")
}

/// Estado de una app en este equipo.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub id: AppId,
    pub name: &'static str,
    pub description: &'static str,
    pub installed: bool,
    pub path: Option<String>,
    pub download_url: String,
    pub latest_version: Option<String>,
}

fn home() -> Option<PathBuf> {
    dirs::home_dir()
}

/// Busca el ejecutable de la app en las ubicaciones habituales del sistema.
pub fn locate(app: &AppDef) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "linux") {
        candidates.push(PathBuf::from("/usr/bin").join(app.linux_bin));
        candidates.push(PathBuf::from("/usr/local/bin").join(app.linux_bin));
        if let Some(h) = home() {
            candidates.push(h.join(".local/bin").join(app.linux_bin));
        }
        candidates.push(PathBuf::from("/opt").join(app.product).join(app.linux_bin));
        if let Some(p) = which(app.linux_bin) {
            candidates.push(p);
        }
    } else if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications").join(format!("{}.app", app.product)));
        if let Some(h) = home() {
            candidates.push(h.join("Applications").join(format!("{}.app", app.product)));
        }
    } else if cfg!(target_os = "windows") {
        let exe = format!("{}.exe", app.windows_exe);
        for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(base) = std::env::var(var) {
                let base = PathBuf::from(base);
                candidates.push(base.join("Programs").join(app.product).join(&exe));
                candidates.push(base.join(app.product).join(&exe));
            }
        }
        if let Some(p) = which(&exe) {
            candidates.push(p);
        }
    }
    candidates.into_iter().find(|p| p.exists())
}

fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Página de descarga (la última release del repositorio).
pub fn download_url(app: &AppDef) -> String {
    format!("https://github.com/{}/releases/latest", app.repo)
}

/// Estado de instalación de todas las apps (sin red).
pub fn installed() -> Vec<AppStatus> {
    APPS.iter()
        .map(|a| {
            let path = locate(a);
            AppStatus {
                id: a.id,
                name: a.name,
                description: a.description,
                installed: path.is_some(),
                path: path.map(|p| p.to_string_lossy().into_owned()),
                download_url: download_url(a),
                latest_version: None,
            }
        })
        .collect()
}

/// Estado de instalación más la última versión publicada en GitHub (con red).
pub async fn status(user_agent: &str) -> Vec<AppStatus> {
    let mut list = installed();
    for s in list.iter_mut() {
        let d = def(s.id);
        s.latest_version = releases::latest_version(d.repo, user_agent).await.ok().flatten();
    }
    list
}

/// Lanza la app con argumentos (rutas de archivo, normalmente). Si no está
/// instalada, devuelve error con la URL de descarga en el mensaje.
pub fn launch(id: AppId, args: &[String]) -> Result<()> {
    let app = def(id);
    let path = locate(app).ok_or_else(|| anyhow!("{} no está instalada. Descárgala en {}", app.name, download_url(app)))?;
    if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg("-a").arg(&path);
        if !args.is_empty() {
            c.arg("--args").args(args);
        }
        c.spawn().with_context(|| format!("no se pudo abrir {}", app.name))?;
    } else {
        Command::new(&path)
            .args(args)
            .spawn()
            .with_context(|| format!("no se pudo abrir {}", app.name))?;
    }
    Ok(())
}

/// Abre un archivo con la app (equivale a `launch` con una sola ruta).
pub fn open_with(id: AppId, file: &Path) -> Result<()> {
    launch(id, &[file.to_string_lossy().into_owned()])
}


/// Perfil de IureDav (unidad WebDAV montada) tal como lo guarda `perfiles.json`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DavMount {
    pub id: String,
    pub name: String,
    pub url: String,
    pub user: String,
    /// Carpeta local donde IureDav monta la unidad.
    pub mount_point: String,
    /// `true` si la carpeta existe y tiene contenido (la unidad está montada).
    pub mounted: bool,
    pub writable: bool,
}

/// Directorio de configuración de IureDav (`ProjectDirs::from("com", "Iurefficient", "IureDav")`):
/// Linux `~/.config/iuredav`, macOS `~/Library/Application Support/com.Iurefficient.IureDav`,
/// Windows `%APPDATA%\Iurefficient\IureDav\config`.
pub fn iuredav_config_dir() -> Option<PathBuf> {
    if cfg!(target_os = "linux") {
        dirs::config_dir().map(|d| d.join("iuredav"))
    } else if cfg!(target_os = "macos") {
        dirs::config_dir().map(|d| d.join("com.Iurefficient.IureDav"))
    } else {
        dirs::config_dir().map(|d| d.join("Iurefficient").join("IureDav").join("config"))
    }
}

/// Perfiles de IureDav configurados en este equipo (sin red). Vacío si IureDav no
/// está instalado o no tiene perfiles.
pub fn iuredav_mounts() -> Vec<DavMount> {
    let Some(path) = iuredav_config_dir().map(|d| d.join("perfiles.json")) else { return vec![] };
    let Ok(text) = std::fs::read_to_string(&path) else { return vec![] };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return vec![] };
    let items = match &v {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(o) => o.get("perfiles").and_then(|p| p.as_array()).cloned().unwrap_or_default(),
        _ => vec![],
    };
    items
        .iter()
        .filter_map(|p| {
            let s = |k: &str| p.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            let mount_point = s("punto_montaje");
            if mount_point.is_empty() {
                return None;
            }
            let mounted = std::fs::read_dir(&mount_point).map(|mut d| d.next().is_some()).unwrap_or(false);
            Some(DavMount {
                id: s("id"),
                name: s("nombre"),
                url: s("url"),
                user: s("usuario"),
                mount_point,
                mounted,
                writable: p.get("escritura").and_then(|x| x.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogo_y_urls() {
        assert_eq!(APPS.len(), 4);
        assert_eq!(AppId::parse("IureEditor"), Some(AppId::Editor));
        assert_eq!(AppId::parse("iureditor"), Some(AppId::Editor));
        assert_eq!(AppId::parse("editor"), Some(AppId::Editor));
        assert_eq!(AppId::parse("IureTranscribe"), Some(AppId::Transcribe));
        assert_eq!(AppId::parse("iuredav"), Some(AppId::Dav));
        assert_eq!(AppId::parse("IureOCR"), Some(AppId::Ocr));
        assert_eq!(download_url(def(AppId::Ocr)), "https://github.com/ellaguno/iureocr/releases/latest");
        assert_eq!(download_url(def(AppId::Dav)), "https://github.com/ellaguno/iuredav/releases/latest");
        assert_eq!(installed().len(), 4);
    }
}

//! Asistentes de IA que pueden usar el servidor MCP local de una app (`<App> --mcp`).
//!
//! - **Claude Desktop**: se registra la app en `claude_desktop_config.json`
//!   (`mcpServers.<clave>`), tocando sólo esa entrada.
//! - **VS Code (GitHub Copilot)**: no se escribe su configuración; se genera el enlace
//!   `vscode:mcp/install?…` y es VS Code quien pregunta al usuario y la guarda. Aquí sólo
//!   se lee para saber si ya está.
//!
//! Microsoft 365 Copilot no aparece aquí porque no usa servidores locales: lo que puede
//! usar es el servidor MCP de la instancia (ver `api::create_mcp_token`).

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::tr;

pub const CLAUDE_DOWNLOAD_URL: &str = "https://claude.ai/download";
pub const VSCODE_DOWNLOAD_URL: &str = "https://code.visualstudio.com/download";
const CLAUDE_CONFIG_FILE: &str = "claude_desktop_config.json";

/// Estado de una app en un asistente.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// El asistente está en este equipo.
    pub installed: bool,
    /// La app está registrada y apunta a este ejecutable.
    pub connected: bool,
    /// Está registrada pero con otra ruta (la app se movió o se reinstaló).
    pub stale: bool,
    pub config_path: Option<String>,
    pub download_url: &'static str,
    /// Sólo VS Code: si tiene la extensión de GitHub Copilot Chat.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copilot: Option<bool>,
}

/// El ejecutable que el asistente tiene que lanzar. En un AppImage, `current_exe` es el
/// punto de montaje temporal, que desaparece al cerrar: la ruta estable es la del
/// propio AppImage.
pub fn command() -> Result<String> {
    if let Ok(p) = std::env::var("APPIMAGE") {
        if !p.trim().is_empty() {
            return Ok(p);
        }
    }
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

/// `{"command": …, "args": ["--mcp"]}`, lo que hay que registrar en un cliente MCP.
pub fn server_entry() -> Result<Value> {
    Ok(json!({"command": command()?, "args": ["--mcp"]}))
}

/// Lee un JSON con comentarios y comas finales (JSONC, como el `mcp.json` de VS Code).
fn parse_jsonc(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str(text) {
        return Some(v);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    // Comas finales: `,` seguida sólo de espacios y `}` o `]`.
    let mut clean = String::with_capacity(out.len());
    let bytes: Vec<char> = out.chars().collect();
    let mut in_str = false;
    for (i, &c) in bytes.iter().enumerate() {
        if c == '"' && (i == 0 || bytes[i - 1] != '\\') {
            in_str = !in_str;
        }
        if c == ',' && !in_str {
            let next = bytes[i + 1..].iter().find(|x| !x.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        clean.push(c);
    }
    serde_json::from_str(&clean).ok()
}

fn read_object(path: &Path) -> Result<Map<String, Value>> {
    if !path.is_file() {
        return Ok(Map::new());
    }
    let text = std::fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        _ => Err(anyhow!(tr!(
            "The configuration is not valid JSON, so it was left untouched: {}",
            "La configuración no es JSON válido; no se tocó: {}",
            path.display()
        ))),
    }
}

/// Escribe con copia previa (`.bak-<clave>`) y de forma atómica: si algo falla a medias,
/// el asistente no se queda con medio archivo.
fn write_object(path: &Path, config: &Map<String, Value>, key: &str) -> Result<()> {
    if path.is_file() {
        let _ = std::fs::copy(path, path.with_extension(format!("json.bak-{key}")));
    }
    let tmp = path.with_extension(format!("json.tmp-{key}"));
    let err = || tr!("Could not write {}", "No se pudo escribir {}", path.display());
    std::fs::write(&tmp, serde_json::to_string_pretty(config)? + "\n").with_context(err)?;
    std::fs::rename(&tmp, path).with_context(err)?;
    Ok(())
}

/// `connected`/`stale` a partir del comando registrado (si lo hay).
fn compare(registered: Option<String>) -> (bool, bool) {
    let ours = command().ok();
    match registered {
        Some(r) => (Some(&r) == ours.as_ref(), Some(&r) != ours.as_ref()),
        None => (false, false),
    }
}

// ---------------------------------------------------------------------------
// Claude Desktop
// ---------------------------------------------------------------------------

/// Carpetas de configuración de Claude Desktop que existen en este equipo.
///
/// La normal es `<config>/Claude` (`%APPDATA%\Claude`, `~/Library/Application
/// Support/Claude`, `~/.config/Claude`). En Windows, la versión de la Microsoft Store
/// (MSIX) redirige `%APPDATA%` a su propia carpeta dentro de `Packages`, y ahí es donde
/// de verdad lee; se escriben las dos si existen.
fn claude_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(d) = dirs::config_dir().map(|d| d.join("Claude")) {
        if d.is_dir() {
            v.push(d);
        }
    }
    #[cfg(windows)]
    if let Some(pkgs) = dirs::data_local_dir().map(|d| d.join("Packages")) {
        if let Ok(entries) = std::fs::read_dir(&pkgs) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("Claude_") {
                    let d = e.path().join("LocalCache").join("Roaming").join("Claude");
                    if d.is_dir() {
                        v.push(d);
                    }
                }
            }
        }
    }
    v
}

pub fn claude_desktop_status(key: &str) -> Status {
    let dirs = claude_dirs();
    let Some(dir) = dirs.first() else {
        return Status { installed: false, connected: false, stale: false, config_path: None, download_url: CLAUDE_DOWNLOAD_URL, copilot: None };
    };
    let path = dir.join(CLAUDE_CONFIG_FILE);
    let registered = read_object(&path).ok().and_then(|m| m.get("mcpServers")?.get(key)?.get("command")?.as_str().map(String::from));
    let (connected, stale) = compare(registered);
    Status { installed: true, connected, stale, config_path: Some(path.to_string_lossy().into_owned()), download_url: CLAUDE_DOWNLOAD_URL, copilot: None }
}

/// Añade (o corrige) `mcpServers.<key>`. Devuelve en cuántos archivos se escribió.
pub fn claude_desktop_connect(key: &str) -> Result<usize> {
    let dirs = claude_dirs();
    if dirs.is_empty() {
        return Err(anyhow!(tr!("Claude Desktop was not found on this computer.", "No se encontró Claude Desktop en este equipo.")));
    }
    // Todo se valida antes de escribir nada: o se actualizan todas las copias o ninguna.
    let mut configs = Vec::new();
    for d in &dirs {
        let path = d.join(CLAUDE_CONFIG_FILE);
        configs.push((read_object(&path)?, path));
    }
    let entry = server_entry()?;
    for (mut config, path) in configs {
        let servers = config.entry("mcpServers").or_insert_with(|| json!({}));
        if !servers.is_object() {
            *servers = json!({});
        }
        servers[key] = entry.clone();
        write_object(&path, &config, key)?;
        log::info!("Claude Desktop: {key} registrado en {}", path.display());
    }
    Ok(dirs.len())
}

/// Quita `mcpServers.<key>`, si está. El resto de la configuración no se toca.
pub fn claude_desktop_disconnect(key: &str) -> Result<()> {
    for d in claude_dirs() {
        let path = d.join(CLAUDE_CONFIG_FILE);
        let mut config = read_object(&path)?;
        let removed = config.get_mut("mcpServers").and_then(Value::as_object_mut).and_then(|s| s.remove(key)).is_some();
        if removed {
            write_object(&path, &config, key)?;
            log::info!("Claude Desktop: {key} quitado de {}", path.display());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// VS Code (GitHub Copilot)
// ---------------------------------------------------------------------------

/// Carpeta de datos de usuario de VS Code (estable), si está instalado.
fn vscode_user_dir() -> Option<PathBuf> {
    let d = dirs::config_dir()?.join("Code").join("User");
    d.is_dir().then_some(d)
}

/// ¿Tiene instalada la extensión de GitHub Copilot Chat? (`~/.vscode/extensions`).
fn vscode_has_copilot() -> bool {
    let Some(dir) = dirs::home_dir().map(|h| h.join(".vscode").join("extensions")) else { return false };
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().any(|e| e.file_name().to_string_lossy().to_ascii_lowercase().starts_with("github.copilot")))
        .unwrap_or(false)
}

/// Estado en VS Code: se lee el `mcp.json` del usuario (clave `servers`).
pub fn vscode_status(key: &str) -> Status {
    let Some(user) = vscode_user_dir() else {
        return Status { installed: false, connected: false, stale: false, config_path: None, download_url: VSCODE_DOWNLOAD_URL, copilot: None };
    };
    let path = user.join("mcp.json");
    let registered = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| parse_jsonc(&t))
        .and_then(|v| v.get("servers")?.get(key)?.get("command")?.as_str().map(String::from));
    let (connected, stale) = compare(registered);
    Status {
        installed: true,
        connected,
        stale,
        config_path: Some(path.to_string_lossy().into_owned()),
        download_url: VSCODE_DOWNLOAD_URL,
        copilot: Some(vscode_has_copilot()),
    }
}

/// Enlace `vscode:mcp/install?…`: al abrirlo, VS Code muestra el servidor y pide
/// confirmación antes de guardarlo en la configuración del usuario.
pub fn vscode_install_url(key: &str) -> Result<String> {
    let mut entry = server_entry()?;
    entry["name"] = json!(key);
    entry["type"] = json!("stdio");
    let encoded: String = url::form_urlencoded::byte_serialize(entry.to_string().as_bytes()).collect();
    // `byte_serialize` codifica el espacio como `+`; en la ruta de un URI tiene que ser %20.
    Ok(format!("vscode:mcp/install?{}", encoded.replace('+', "%20")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_con_comentarios_y_comas_finales() {
        let t = r#"{
            // servidores
            "servers": {
                "iureocr": {"command": "C:\\Apps\\IureOCR.exe", "args": ["--mcp",],}, /* fin */
                "url": {"command": "http://x//y"},
            },
        }"#;
        let v = parse_jsonc(t).expect("jsonc");
        assert_eq!(v["servers"]["iureocr"]["command"], "C:\\Apps\\IureOCR.exe");
        assert_eq!(v["servers"]["url"]["command"], "http://x//y");
    }

    #[test]
    fn enlace_de_vscode() {
        let url = vscode_install_url("iureocr").unwrap();
        assert!(url.starts_with("vscode:mcp/install?%7B"));
        assert!(!url.contains('+') && !url.contains(' '));
        let json: String = url::form_urlencoded::parse(url.split_once('?').unwrap().1.replace("%20", "+").as_bytes())
            .map(|(k, _)| k.into_owned())
            .collect();
        let v: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["name"], "iureocr");
        assert_eq!(v["args"][0], "--mcp");
    }

    /// Conserva lo que ya había, en su orden, y sólo cambia nuestra entrada.
    #[test]
    fn claude_conserva_el_resto_de_la_configuracion() {
        let dir = std::env::temp_dir().join("iurefficient-connect-agents-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CLAUDE_CONFIG_FILE);
        std::fs::write(&path, r#"{"zPreferencia": true, "mcpServers": {"otro": {"command": "x"}}, "aOtra": 1}"#).unwrap();

        let mut config = read_object(&path).unwrap();
        config["mcpServers"]["iureocr"] = server_entry().unwrap();
        write_object(&path, &config, "iureocr").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["mcpServers"]["otro"]["command"], "x");
        assert_eq!(v["mcpServers"]["iureocr"]["args"][0], "--mcp");
        assert!(text.find("zPreferencia").unwrap() < text.find("aOtra").unwrap(), "se perdió el orden");
        assert!(path.with_extension("json.bak-iureocr").is_file());

        std::fs::write(&path, "{ esto no es json").unwrap();
        assert!(read_object(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Conectar, ver el estado y desconectar sobre una carpeta de configuración falsa
    /// (`XDG_CONFIG_HOME`), sin tocar la de verdad.
    #[cfg(target_os = "linux")]
    #[test]
    fn claude_conectar_y_desconectar() {
        let home = std::env::temp_dir().join("iurefficient-connect-xdg-test");
        let _ = std::fs::remove_dir_all(&home);
        std::env::set_var("XDG_CONFIG_HOME", &home);

        assert!(!claude_desktop_status("iureocr").installed);
        assert!(claude_desktop_connect("iureocr").is_err(), "sin Claude Desktop no debe crear nada");
        assert!(!vscode_status("iureocr").installed);

        let dir = home.join("Claude");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CLAUDE_CONFIG_FILE);
        std::fs::write(&path, r#"{"mcpServers": {"otro": {"command": "x"}}}"#).unwrap();
        let s = claude_desktop_status("iureocr");
        assert!(s.installed && !s.connected && !s.stale);

        assert_eq!(claude_desktop_connect("iureocr").unwrap(), 1);
        assert!(claude_desktop_status("iureocr").connected);

        // Otra ruta registrada = desactualizado.
        let mut config = read_object(&path).unwrap();
        config["mcpServers"]["iureocr"]["command"] = json!("/otra/ruta/IureOCR");
        write_object(&path, &config, "iureocr").unwrap();
        let s = claude_desktop_status("iureocr");
        assert!(s.stale && !s.connected);

        claude_desktop_disconnect("iureocr").unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(v["mcpServers"].get("iureocr").is_none());
        assert_eq!(v["mcpServers"]["otro"]["command"], "x");

        // VS Code: sólo se lee su mcp.json (con comentarios).
        let user = home.join("Code").join("User");
        std::fs::create_dir_all(&user).unwrap();
        assert!(!vscode_status("iureocr").connected);
        std::fs::write(
            user.join("mcp.json"),
            format!("{{\n  // mío\n  \"servers\": {{\"iureocr\": {{\"command\": {}, \"args\": [\"--mcp\"]}},}}\n}}", json!(command().unwrap())),
        )
        .unwrap();
        assert!(vscode_status("iureocr").connected);
        let _ = std::fs::remove_dir_all(&home);
    }
}

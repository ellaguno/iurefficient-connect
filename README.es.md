[Read in English](README.md)

# iurefficient-connect

Conector común, en Rust, de las aplicaciones de escritorio de Iurefficient
([IureTranscribe](https://github.com/ellaguno/iuretranscribe),
[IureEditor](https://github.com/ellaguno/iureditor), [IureDav](https://github.com/ellaguno/iuredav),
[IureOCR](https://github.com/ellaguno/iureocr))
con una instancia de [Iurefficient](https://iurefficient.com). Sin dependencias de
Tauri ni de ninguna interfaz: cada app pone su propia pantalla encima.

| Módulo | Qué hace | Credencial |
| --- | --- | --- |
| `account` | Normaliza el dominio (`2.ds.iurefficient.com` → `https://2.ds.iurefficient.com/`) e identifica al usuario | — |
| `secrets` | Guarda y lee credenciales en el llavero del sistema, **compartidas entre las apps** (Secret Service, Llaveros de macOS, Administrador de credenciales de Windows) | — |
| `webdav` | Árbol de documentos: listar carpetas con permisos, subir (con progreso, reintento como `.txt` si la instancia rechaza `.srt`/`.vtt`), descargar | contraseña de aplicación `iurdav_…` |
| `mcp` | Servidor MCP de sólo lectura: `listar_proyectos`, `buscar_en_expedientes`, `detalle_proyecto`, … | token `iurmcp_…` |
| `rest` | API REST con sesión de cookies (JWT + CSRF), inicio de sesión con TOTP opcional, refresco automático del acceso, sesión exportable al llavero | correo y contraseña |
| `api` | Operaciones tipadas sobre la sesión REST: proyectos (`cases`), documentos (subir, listar, descargar), minutas por blueprint (`ai_options`, `compose`, `compose_status`), compromisos → tareas, horas (`add_time_entry`), CRM (leads, oportunidades, adjuntos, actividades), chat con IA (`global_chat`) y **contraseñas de aplicación WebDAV** (`create_webdav_token`: la app inicia sesión con la cuenta y crea la `iurdav_…` sin que el usuario la vea) | sesión REST |
| `releases` | Aviso de versiones nuevas publicadas en GitHub | — |
| `lang` | Idioma de la interfaz compartido por las apps: inglés por defecto, español si el sistema está en español; todos los mensajes del conector (y la descripción de las apps en `apps`) salen en ese idioma. Macro `tr!(inglés, español, …)` para los textos de las apps | — |
| `apps` | Catálogo de las cuatro apps: detecta cuáles están instaladas (Linux, Windows, macOS), las lanza con argumentos («Abrir con IureEditor») y consulta su última versión | — |

## Uso

```rust
use iurefficient_connect::{Account, webdav::WebDav, user_agent};

let acc = Account::new("2.ds.iurefficient.com", "yo@despacho.com")?;
let dav = WebDav::new(acc.clone(), "iurdav_…", &user_agent("MiApp", "1.0.0"))?;
let raiz = dav.list("").await?;                       // Clientes, General, Vistas (solo navegar)
let subido = dav.upload_with_fallback("Clientes/Acme/Proyecto 1", path, |enviado, total| {}).await?;
```

```rust
use iurefficient_connect::rest::{Session, Login};

let s = Session::new(acc, &user_agent("MiApp", "1.0.0"))?;
match s.login("contraseña").await? {
    Login::Ok(user) => { /* sesión abierta */ }
    Login::TotpRequired { totp_token } => { s.verify_totp(&totp_token, "123456").await?; }
}
secrets::guardar(&acc, secrets::Kind::Session, &serde_json::to_string(&s.export())?)?;
```

```rust
use iurefficient_connect::{lang, tr};

// Al arrancar y cada vez que cambia el ajuste («auto», «en» o «es»):
lang::set(lang::resolve(&settings.ui_language));

// Textos propios de la app: primero inglés, después español.
let msg = tr!("Uploaded {name}", "Se subió {name}");
let etiqueta = lang::pick("Settings", "Ajustes");
```

## Reglas del servidor que el conector respeta

- `PUT` a una ruta nueva crea un documento (201); a una existente, una versión (204).
- `DELETE`, `MOVE`, `COPY` y `MKCOL` devuelven 405 por diseño: el ciclo de vida lo gobierna la aplicación.
- `Vistas (solo navegar)/` no admite escrituras.
- Las instancias no admiten `.srt`/`.vtt` por defecto; el conector reintenta como `.txt`.
- El JWT vive en cookies httpOnly (acceso 1 h, refresco 30 días) con CSRF de doble envío.

## Pruebas

```bash
cargo test
# Integración contra el doble de pruebas de IureDav (o una instancia real):
python3 ../iuredav/tests/servidor-falso.py 8099 &
IURE_TEST_URL=http://127.0.0.1:8099 IURE_TEST_USER=prueba@ejemplo.com IURE_TEST_PASS=iurdav_falso cargo test -- integration --nocapture
```

Sin el feature `keyring` (`--no-default-features`) el crate no necesita D-Bus en Linux.

## Licencia

MIT.

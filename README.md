[Leer en español](README.es.md)

# iurefficient-connect

Shared Rust connector between the Iurefficient desktop apps
([IureTranscribe](https://github.com/ellaguno/iuretranscribe),
[IureEditor](https://github.com/ellaguno/iureditor), [IureDav](https://github.com/ellaguno/iuredav),
[IureOCR](https://github.com/ellaguno/iureocr))
and an [Iurefficient](https://iurefficient.com) instance. No dependency on Tauri or
any UI: each app puts its own screens on top.

| Module | What it does | Credential |
| --- | --- | --- |
| `account` | Normalizes the domain (`2.ds.iurefficient.com` → `https://2.ds.iurefficient.com/`) and identifies the user | — |
| `secrets` | Stores and reads credentials in the system keyring, **shared between the apps** (Secret Service, macOS Keychain, Windows Credential Manager) | — |
| `webdav` | Document tree: list folders with permissions, upload (with progress, retrying as `.txt` if the instance rejects `.srt`/`.vtt`), download | app password `iurdav_…` |
| `mcp` | Read-only MCP server: `listar_proyectos`, `buscar_en_expedientes`, `detalle_proyecto`, … | token `iurmcp_…` |
| `rest` | REST API with a cookie session (JWT + CSRF), sign-in with optional TOTP, automatic access refresh, session exportable to the keyring | email and password |
| `api` | Typed operations over the REST session: projects (`cases`), documents (upload, list, download), minutes by blueprint (`ai_options`, `compose`, `compose_status`), commitments → tasks, hours (`add_time_entry`), CRM (leads, opportunities, attachments, activities), AI chat (`global_chat`) and **WebDAV app passwords** (`create_webdav_token`: the app signs in with the account and creates the `iurdav_…` password without the user seeing it) | REST session |
| `releases` | Notice of new versions published on GitHub | — |
| `lang` | UI language shared by the apps: English by default, Spanish when the OS is in Spanish; every connector message (and the app descriptions in `apps`) comes out in that language. `tr!(english, spanish, …)` macro for the apps' own texts | — |
| `apps` | Catalog of the four apps: detects which are installed (Linux, Windows, macOS), launches them with arguments ("Open with IureEditor") and checks their latest version | — |

## Usage

```rust
use iurefficient_connect::{Account, webdav::WebDav, user_agent};

let acc = Account::new("2.ds.iurefficient.com", "me@firm.com")?;
let dav = WebDav::new(acc.clone(), "iurdav_…", &user_agent("MyApp", "1.0.0"))?;
let root = dav.list("").await?;                       // Clientes, General, Vistas (solo navegar)
let uploaded = dav.upload_with_fallback("Clientes/Acme/Proyecto 1", path, |sent, total| {}).await?;
```

```rust
use iurefficient_connect::rest::{Session, Login};

let s = Session::new(acc, &user_agent("MyApp", "1.0.0"))?;
match s.login("password").await? {
    Login::Ok(user) => { /* session open */ }
    Login::TotpRequired { totp_token } => { s.verify_totp(&totp_token, "123456").await?; }
}
secrets::guardar(&acc, secrets::Kind::Session, &serde_json::to_string(&s.export())?)?;
```

```rust
use iurefficient_connect::{lang, tr};

// At startup and whenever the setting ("auto", "en" or "es") changes:
lang::set(lang::resolve(&settings.ui_language));

// The app's own texts: English first, then Spanish.
let msg = tr!("Uploaded {name}", "Se subió {name}");
let label = lang::pick("Settings", "Ajustes");
```

## Server rules the connector respects

- `PUT` to a new path creates a document (201); to an existing one, a version (204).
- `DELETE`, `MOVE`, `COPY` and `MKCOL` return 405 by design: the application governs the lifecycle.
- `Vistas (solo navegar)/` does not accept writes.
- Instances do not accept `.srt`/`.vtt` by default; the connector retries as `.txt`.
- The JWT lives in httpOnly cookies (access 1 h, refresh 30 days) with double-submit CSRF.

## Tests

```bash
cargo test
# Integration against IureDav's test double (or a real instance):
python3 ../iuredav/tests/servidor-falso.py 8099 &
IURE_TEST_URL=http://127.0.0.1:8099 IURE_TEST_USER=prueba@ejemplo.com IURE_TEST_PASS=iurdav_falso cargo test -- integration --nocapture
```

Without the `keyring` feature (`--no-default-features`) the crate does not need D-Bus on Linux.

## License

MIT.

//! Idioma de la interfaz, compartido por las apps: inglés por defecto y español
//! cuando el sistema operativo está en español (o el usuario lo elige).
//!
//! Cada app llama a [`set`] al arrancar y al cambiar su ajuste de idioma; los
//! mensajes del conector (y los de la app que usen [`tr!`](crate::tr)) salen
//! entonces en ese idioma.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    En,
    Es,
}

impl Lang {
    /// Código ISO 639-1 (`"en"` / `"es"`).
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0); // 0 = En, 1 = Es

/// Idioma del sistema: español si la primera preferencia del usuario es `es*`;
/// cualquier otro idioma, o no poder saberlo, da inglés.
pub fn detect_os() -> Lang {
    from_locale(sys_locale::get_locale().as_deref())
}

fn from_locale(locale: Option<&str>) -> Lang {
    match locale {
        Some(l) if l.trim().to_ascii_lowercase().starts_with("es") => Lang::Es,
        _ => Lang::En,
    }
}

/// Resuelve el ajuste de una app: `"en"`, `"es"` o cualquier otra cosa (`"auto"`,
/// vacío) = idioma del sistema.
pub fn resolve(preference: &str) -> Lang {
    match preference.trim().to_ascii_lowercase().as_str() {
        "en" => Lang::En,
        "es" => Lang::Es,
        _ => detect_os(),
    }
}

/// Fija el idioma de los mensajes (de todo el proceso).
pub fn set(lang: Lang) {
    CURRENT.store(if lang == Lang::Es { 1 } else { 0 }, Ordering::Relaxed);
}

/// Idioma actual de los mensajes (inglés hasta que la app llame a [`set`]).
pub fn current() -> Lang {
    if CURRENT.load(Ordering::Relaxed) == 1 {
        Lang::Es
    } else {
        Lang::En
    }
}

/// Elige entre dos textos fijos según el idioma actual.
pub fn pick(en: &'static str, es: &'static str) -> &'static str {
    match current() {
        Lang::En => en,
        Lang::Es => es,
    }
}

/// Texto traducido con formato: `tr!("Cannot open {path}", "No se pudo abrir {path}")`,
/// o con argumentos posicionales: `tr!("{} not found", "No se encontró {}", name)`.
/// Primero inglés, después español; devuelve `String`.
#[macro_export]
macro_rules! tr {
    ($en:literal, $es:literal $(, $arg:expr)* $(,)?) => {
        match $crate::lang::current() {
            $crate::lang::Lang::En => format!($en $(, $arg)*),
            $crate::lang::Lang::Es => format!($es $(, $arg)*),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locale_detection() {
        assert_eq!(from_locale(Some("es-MX")), Lang::Es);
        assert_eq!(from_locale(Some("es_ES.UTF-8")), Lang::Es);
        assert_eq!(from_locale(Some("en-US")), Lang::En);
        assert_eq!(from_locale(Some("fr-FR")), Lang::En);
        assert_eq!(from_locale(Some("C")), Lang::En);
        assert_eq!(from_locale(None), Lang::En);
        assert_eq!(resolve("es"), Lang::Es);
        assert_eq!(resolve("EN"), Lang::En);
    }

    #[test]
    fn tr_macro() {
        let name = "x";
        set(Lang::Es);
        assert_eq!(crate::tr!("{name} missing", "falta {name}"), "falta x");
        set(Lang::En);
        assert_eq!(crate::tr!("{} missing", "falta {}", name), "x missing");
    }
}

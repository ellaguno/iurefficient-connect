//! Aviso de versiones nuevas publicadas como releases en GitHub. Sólo consulta:
//! sin firma de código, instalar desde dentro no tiene sentido.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::lang;

#[derive(Debug, Clone)]
pub struct Release {
    pub version: String,
    pub url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
}

fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let mut it = v.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).map(|s| s.parse::<u64>().ok());
    Some((it.next()??, it.next()??, it.next().flatten().unwrap_or(0)))
}

/// `true` si `ultima` es una versión posterior a `actual` (semver simple).
pub fn es_mas_nueva(actual: &str, ultima: &str) -> bool {
    match (parse(actual), parse(ultima)) {
        (Some(a), Some(u)) => u > a,
        _ => false,
    }
}

/// Última versión publicada (sin comparar con nada); `Ok(None)` si no hay releases.
pub async fn latest_version(owner_repo: &str, user_agent: &str) -> Result<Option<String>> {
    let url = format!("https://api.github.com/repos/{owner_repo}/releases/latest");
    let resp = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| lang::pick("could not reach GitHub", "no se pudo consultar GitHub"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let r: GhRelease = resp.error_for_status()?.json().await?;
    Ok(Some(r.tag_name.trim_start_matches('v').to_string()))
}

/// Consulta la última release de `owner/repo`; `Ok(None)` si no hay una más nueva.
pub async fn consultar(owner_repo: &str, actual: &str, user_agent: &str) -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{owner_repo}/releases/latest");
    let r: GhRelease = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(15))
        .build()?
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| lang::pick("could not reach GitHub", "no se pudo consultar GitHub"))?
        .error_for_status()?
        .json()
        .await?;
    if r.draft || r.prerelease || !es_mas_nueva(actual, &r.tag_name) {
        return Ok(None);
    }
    Ok(Some(Release { version: r.tag_name.trim_start_matches('v').to_string(), url: r.html_url }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compara_versiones() {
        assert!(es_mas_nueva("0.2.2", "v0.3.0"));
        assert!(es_mas_nueva("0.2.2", "0.2.10"));
        assert!(!es_mas_nueva("0.2.2", "0.2.2"));
        assert!(!es_mas_nueva("1.0.0", "0.9.9"));
        assert!(!es_mas_nueva("x", "0.1.0"));
    }
}

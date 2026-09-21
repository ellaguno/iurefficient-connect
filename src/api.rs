//! Operaciones tipadas sobre la API REST de la instancia (encima de [`crate::rest::Session`]).
//! Contratos tomados del código de la instancia v4.86 (rutas `cases`, `documents`,
//! `blueprints`, `time_tracking`, `system_settings` y el plugin `iur_crm`).

use anyhow::{anyhow, Context, Result};
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

use crate::rest::Session;

fn s(v: &Value, k: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}
fn opt(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(str::to_string)
}

/// Terminología de la instancia (`caso`/`proyecto`, `cliente`/`paciente`…).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Terminology {
    pub case: String,
    pub cases: String,
    pub client: String,
    pub clients: String,
    #[serde(default)]
    pub specialty: Option<String>,
}

pub async fn terminology(sess: &Session) -> Result<Terminology> {
    let v = sess.get_json("/api/system-settings/terminology").await?;
    let t = v.get("terminology").cloned().unwrap_or(Value::Null);
    let pick = |k: &str, d: &str| opt(&t, k).filter(|x| !x.is_empty()).unwrap_or_else(|| d.to_string());
    Ok(Terminology {
        case: pick("case", "proyecto"),
        cases: pick("cases", "proyectos"),
        client: pick("client", "cliente"),
        clients: pick("clients", "clientes"),
        specialty: opt(&t, "specialty"),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaseSummary {
    pub id: String,
    pub case_number: String,
    pub title: String,
    pub status: String,
    pub client_name: Option<String>,
    pub client_id: Option<String>,
}

fn case_from(v: &Value) -> CaseSummary {
    let client = v.get("client").filter(|c| !c.is_null());
    CaseSummary {
        id: s(v, "id"),
        case_number: s(v, "case_number"),
        title: s(v, "title"),
        status: s(v, "status"),
        client_name: client.and_then(|c| opt(c, "display_name").or_else(|| opt(c, "company_name")).or_else(|| {
            let n = format!("{} {}", s(c, "first_name"), s(c, "last_name"));
            let n = n.trim().to_string();
            (!n.is_empty()).then_some(n)
        })),
        client_id: opt(v, "client_id"),
    }
}

/// `GET /api/cases?search=&exclude_archived=true` (paginado; devuelve hasta `limit`, máx. 200).
pub async fn cases(sess: &Session, search: Option<&str>, limit: u32) -> Result<Vec<CaseSummary>> {
    let mut path = format!("/api/cases?exclude_archived=true&paginate=true&page=1&limit={}", limit.clamp(1, 200));
    if let Some(q) = search.map(str::trim).filter(|q| !q.is_empty()) {
        path.push_str(&format!("&search={}", urlencode(q)));
    }
    let v = sess.get_json(&path).await?;
    Ok(v.get("cases").and_then(|c| c.as_array()).map(|a| a.iter().map(case_from).collect()).unwrap_or_default())
}

fn urlencode(q: &str) -> String {
    url::form_urlencoded::byte_serialize(q.as_bytes()).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub id: String,
    pub title: String,
    pub file_name: String,
    pub document_type: String,
    pub version: Option<u64>,
    pub transcription_status: Option<String>,
    pub is_version: bool,
}

fn document_from(v: &Value) -> Document {
    Document {
        id: s(v, "id"),
        title: s(v, "title"),
        file_name: s(v, "file_name"),
        document_type: s(v, "document_type"),
        version: v.get("version").and_then(|x| x.as_u64()),
        transcription_status: opt(v, "transcription_status"),
        is_version: false,
    }
}

/// Parámetros de `POST /api/documents`.
#[derive(Debug, Clone, Default)]
pub struct UploadOptions {
    pub case_id: Option<String>,
    pub client_id: Option<String>,
    pub title: Option<String>,
    /// `other` por omisión en la instancia.
    pub document_type: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// Crear como versión de un documento existente.
    pub as_version_of: Option<String>,
    /// Nombre remoto (por omisión el del archivo local).
    pub file_name: Option<String>,
}

/// Sube un archivo como documento (`POST /api/documents`, multipart). La instancia
/// valida la extensión (`.srt`/`.vtt` no se admiten por defecto: usa [`crate::webdav::fallback_name`]).
pub async fn upload_document(sess: &Session, local: &Path, opts: &UploadOptions) -> Result<Document> {
    let bytes = tokio::fs::read(local).await.with_context(|| format!("No se pudo leer {}", local.display()))?;
    let name = opts.file_name.clone().or_else(|| local.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "archivo".into());
    let mut form = Form::new().part("file", Part::bytes(bytes).file_name(name));
    if let Some(c) = &opts.case_id { form = form.text("case_id", c.clone()); }
    if let Some(c) = &opts.client_id { form = form.text("client_id", c.clone()); }
    if let Some(t) = &opts.title { form = form.text("title", t.clone()); }
    if let Some(t) = &opts.document_type { form = form.text("document_type", t.clone()); }
    if let Some(d) = &opts.description { form = form.text("description", d.clone()); }
    if !opts.tags.is_empty() { form = form.text("tags", serde_json::to_string(&opts.tags)?); }
    if let Some(v) = &opts.as_version_of { form = form.text("as_version_of", v.clone()); }
    let v = sess.post_multipart("/api/documents", form).await.map_err(map_upload_error)?;
    let d = v.get("document").ok_or_else(|| anyhow!("La instancia no devolvió el documento"))?;
    let mut doc = document_from(d);
    doc.is_version = v.get("is_version").and_then(|b| b.as_bool()).unwrap_or(false);
    Ok(doc)
}

fn map_upload_error(e: anyhow::Error) -> anyhow::Error {
    let m = e.to_string();
    if m.contains("File type not allowed") {
        anyhow!("La instancia no admite ese tipo de archivo")
    } else if m.contains("quota") || m.contains("cuota") {
        anyhow!("Se alcanzó la cuota de almacenamiento del plan")
    } else {
        e
    }
}

/// `GET /api/documents?case_id=`.
pub async fn documents_of_case(sess: &Session, case_id: &str) -> Result<Vec<Document>> {
    let v = sess.get_json(&format!("/api/documents?case_id={}", urlencode(case_id))).await?;
    Ok(v.get("documents").and_then(|d| d.as_array()).map(|a| a.iter().map(document_from).collect()).unwrap_or_default())
}

/// Descarga un documento (`GET /api/documents/<id>/download?download=true`) a una ruta local.
pub async fn download_document(sess: &Session, doc_id: &str, local: &Path) -> Result<u64> {
    let rb = sess.request(reqwest::Method::GET, &format!("/api/documents/{doc_id}/download?download=true")).await?;
    let resp = rb.send().await?;
    if !resp.status().is_success() {
        return Err(anyhow!("No se pudo descargar el documento ({})", resp.status()));
    }
    let bytes = resp.bytes().await?;
    tokio::fs::write(local, &bytes).await?;
    Ok(bytes.len() as u64)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlueprintOption {
    pub id: String,
    pub name: String,
    pub description: String,
    pub genre: String,
    pub section_count: u64,
    pub has_design: bool,
    pub suggested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiOptions {
    pub can_generate: bool,
    pub reason: Option<String>,
    pub is_transcript: bool,
    pub composing: bool,
    pub blueprints: Vec<BlueprintOption>,
}

/// `GET /api/documents/<id>/ai-options`: qué se puede generar con IA a partir del documento.
pub async fn ai_options(sess: &Session, doc_id: &str) -> Result<AiOptions> {
    let v = sess.get_json(&format!("/api/documents/{doc_id}/ai-options")).await?;
    Ok(AiOptions {
        can_generate: v["can_generate"].as_bool().unwrap_or(false),
        reason: opt(&v, "reason"),
        is_transcript: v["is_transcript"].as_bool().unwrap_or(false),
        composing: v["composing"].as_bool().unwrap_or(false),
        blueprints: v["blueprints"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|b| BlueprintOption {
                        id: s(b, "id"),
                        name: s(b, "name"),
                        description: s(b, "description"),
                        genre: s(b, "genre"),
                        section_count: b["section_count"].as_u64().unwrap_or(0),
                        has_design: b["has_design"].as_bool().unwrap_or(false),
                        suggested: b["suggested"].as_bool().unwrap_or(false),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// Parámetros de `POST /api/blueprints/<id>/compose`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ComposeRequest {
    pub case_id: Option<String>,
    pub source_document_ids: Vec<String>,
    pub title: Option<String>,
    pub extra_instructions: Option<String>,
    /// Asistentes (sólo cuando la única fuente es una grabación).
    pub attendees: Vec<String>,
    pub speaker_map: HashMap<String, String>,
    pub force: bool,
}

/// Lanza la composición; devuelve `task_id`.
pub async fn compose(sess: &Session, blueprint_id: &str, req: &ComposeRequest) -> Result<String> {
    let mut body = json!({"source_document_ids": req.source_document_ids, "force": req.force});
    if let Some(c) = &req.case_id { body["case_id"] = json!(c); }
    if let Some(t) = &req.title { body["title"] = json!(t); }
    if let Some(x) = &req.extra_instructions { body["extra_instructions"] = json!(x); }
    if !req.attendees.is_empty() { body["attendees"] = json!(req.attendees); }
    if !req.speaker_map.is_empty() { body["speaker_map"] = json!(req.speaker_map); }
    let v = sess.post_json(&format!("/api/blueprints/{blueprint_id}/compose"), &body).await.map_err(|e| {
        let m = e.to_string();
        if m.contains("ALREADY_COMPOSING") || m.contains("Ya se está generando") {
            anyhow!("Ya se está generando un documento a partir de ese material; espera a que termine")
        } else if m.contains("SOURCE_WITHOUT_TEXT") {
            anyhow!("El documento fuente no tiene texto legible todavía")
        } else {
            e
        }
    })?;
    v.get("task_id").and_then(|t| t.as_str()).map(str::to_string).ok_or_else(|| anyhow!("La instancia no devolvió task_id"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComposeStatus {
    /// PENDING | PROGRESS | SUCCESS | FAILURE
    pub state: String,
    pub current: u64,
    pub total: u64,
    pub section: Option<String>,
    pub document_id: Option<String>,
    pub link: Option<String>,
    pub error: Option<String>,
}

impl ComposeStatus {
    pub fn finished(&self) -> bool {
        matches!(self.state.as_str(), "SUCCESS" | "FAILURE" | "REVOKED")
    }
}

pub async fn compose_status(sess: &Session, task_id: &str) -> Result<ComposeStatus> {
    let v = sess.get_json(&format!("/api/blueprints/compose-status/{task_id}")).await?;
    Ok(ComposeStatus {
        state: s(&v, "state"),
        current: v["current"].as_u64().unwrap_or(0),
        total: v["total"].as_u64().unwrap_or(0),
        section: opt(&v, "section"),
        document_id: opt(&v, "document_id"),
        link: opt(&v, "link"),
        error: opt(&v, "error"),
    })
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Commitment {
    pub title: String,
    pub description: Option<String>,
    pub assignee_name: Option<String>,
    pub assigned_to_id: Option<String>,
    pub due_date: Option<String>,
    pub due_hint: Option<String>,
}

/// `GET /api/documents/<id>/commitments` (compromisos detectados en una minuta).
pub async fn commitments(sess: &Session, doc_id: &str) -> Result<(Vec<Commitment>, Option<String>)> {
    let v = sess.get_json(&format!("/api/documents/{doc_id}/commitments")).await?;
    let list = v["commitments"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| Commitment {
                    title: s(c, "title"),
                    description: opt(c, "description"),
                    assignee_name: opt(c, "assignee_name"),
                    assigned_to_id: opt(c, "assigned_to_id"),
                    due_date: opt(c, "due_date"),
                    due_hint: opt(c, "due_hint"),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((list, opt(&v, "case_id")))
}

/// `POST /api/documents/<id>/commitments/apply`: convierte compromisos en tareas.
pub async fn apply_commitments(sess: &Session, doc_id: &str, items: &[Commitment], case_id: Option<&str>) -> Result<u64> {
    let mut body = json!({"commitments": items});
    if let Some(c) = case_id { body["case_id"] = json!(c); }
    let v = sess.post_json(&format!("/api/documents/{doc_id}/commitments/apply"), &body).await?;
    Ok(v["created"].as_u64().unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Horas
// ---------------------------------------------------------------------------

/// `POST /api/cases/<id>/time-entries`. `entry_date` en `YYYY-MM-DD` (hoy si `None`).
pub async fn add_time_entry(sess: &Session, case_id: &str, hours: f64, description: &str, billable: bool, entry_date: Option<&str>) -> Result<String> {
    let mut body = json!({"hours": hours, "description": description, "billable": billable});
    if let Some(d) = entry_date { body["entry_date"] = json!(d); }
    let v = sess.post_json(&format!("/api/cases/{case_id}/time-entries"), &body).await.map_err(|e| {
        if e.to_string().contains("409") { anyhow!("Esa semana ya fue enviada y no admite horas nuevas") } else { e }
    })?;
    Ok(s(&v["time_entry"], "id"))
}

// ---------------------------------------------------------------------------
// CRM (plugin iur_crm)
// ---------------------------------------------------------------------------

/// `true` si el plugin CRM está habilitado y accesible para el usuario (`GET /api/plugins`).
pub async fn crm_available(sess: &Session) -> Result<bool> {
    let v = sess.get_json("/api/plugins").await?;
    Ok(v["plugins"].as_array().map(|a| a.iter().any(|p| s(p, "id") == "iur-crm")).unwrap_or(false))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CrmKind {
    Lead,
    Opportunity,
}

impl CrmKind {
    fn segment(self) -> &'static str {
        match self {
            CrmKind::Lead => "leads",
            CrmKind::Opportunity => "opportunities",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrmItem {
    pub kind: CrmKind,
    pub id: String,
    pub name: String,
    pub organization: Option<String>,
    pub status: Option<String>,
    pub stage: Option<String>,
    pub amount: Option<f64>,
}

/// Lista leads u oportunidades (`GET /api/plugins/crm/{leads|opportunities}?search=`).
pub async fn crm_list(sess: &Session, kind: CrmKind, search: Option<&str>, limit: u32) -> Result<Vec<CrmItem>> {
    let mut path = format!("/api/plugins/crm/{}?limit={}&offset=0", kind.segment(), limit.clamp(1, 500));
    if let Some(q) = search.map(str::trim).filter(|q| !q.is_empty()) {
        path.push_str(&format!("&search={}", urlencode(q)));
    }
    if kind == CrmKind::Opportunity {
        path.push_str("&status=open");
    }
    let v = sess.get_json(&path).await?;
    Ok(v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| CrmItem {
                    kind,
                    id: s(i, "id"),
                    name: if kind == CrmKind::Lead {
                        let n = s(i, "name");
                        if n.is_empty() { s(i, "company_name") } else { n }
                    } else {
                        s(i, "name")
                    },
                    organization: opt(i, "organization_name").or_else(|| opt(i, "company_name")),
                    status: opt(i, "status"),
                    stage: opt(i, "stage_name"),
                    amount: i["amount"].as_f64(),
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Adjunta un archivo a un lead u oportunidad (`POST …/<id>/documents`, multipart `file`).
pub async fn crm_attach_file(sess: &Session, kind: CrmKind, id: &str, local: &Path, file_name: Option<&str>) -> Result<Document> {
    let bytes = tokio::fs::read(local).await.with_context(|| format!("No se pudo leer {}", local.display()))?;
    let name = file_name.map(str::to_string).or_else(|| local.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_else(|| "archivo".into());
    let form = Form::new().part("file", Part::bytes(bytes).file_name(name));
    let v = sess.post_multipart(&format!("/api/plugins/crm/{}/{id}/documents", kind.segment()), form).await.map_err(map_upload_error)?;
    Ok(document_from(&v))
}

/// Vincula un documento ya existente a un lead u oportunidad.
pub async fn crm_attach_document(sess: &Session, kind: CrmKind, id: &str, document_id: &str) -> Result<Document> {
    let v = sess.post_json(&format!("/api/plugins/crm/{}/{id}/documents", kind.segment()), &json!({"document_id": document_id})).await?;
    Ok(document_from(&v))
}

/// Registra una actividad completada (`POST /api/plugins/crm/activities`).
/// `activity_type`: call | meeting | email | whatsapp | sms | note | task.
pub async fn crm_activity(
    sess: &Session,
    kind: CrmKind,
    id: &str,
    activity_type: &str,
    subject: &str,
    description: Option<&str>,
    duration_minutes: Option<u32>,
    completed_at_iso: Option<&str>,
) -> Result<String> {
    let mut body = json!({"activity_type": activity_type, "subject": subject, "schedule_in_calendar": false, "outcome": "successful"});
    match kind {
        CrmKind::Lead => body["lead_id"] = json!(id),
        CrmKind::Opportunity => body["opportunity_id"] = json!(id),
    }
    if let Some(d) = description { body["description"] = json!(d); }
    if let Some(m) = duration_minutes { body["duration_minutes"] = json!(m); }
    if let Some(c) = completed_at_iso { body["completed_at"] = json!(c); }
    let v = sess.post_json("/api/plugins/crm/activities", &body).await?;
    Ok(s(&v, "id"))
}

//! Operaciones de PIM sobre la API REST de la instancia (encima de [`crate::rest::Session`]):
//! agenda, contactos y clientes, tareas personales, avisos, comentarios y menciones,
//! tickets, correo del CRM y ayudas de IA. Las usa IureDesk; sirven a cualquier app.
//!
//! Contratos tomados del código de la instancia v4.111–4.114 (rutas `calendar-events`,
//! `contacts`, `clients`, `tasks`, `notifications`, `search`, `cases/<id>/ai-summary`, y
//! los plugins `iur-collab`, `iur-tickets` e `iur-crm`). Las fechas se dejan como texto
//! ISO 8601 tal como las da la instancia: los eventos llevan `Z` (instantes en UTC); las
//! fechas de tarea no (son fechas «de pared»).

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::api::{opt, s, urlencode, CrmKind};
use crate::lang;
use crate::rest::Session;

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn strings(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Los ids de la instancia son UUID en texto, pero algunos campos antiguos llegan como número.
fn id_of(v: &Value, k: &str) -> Option<String> {
    match v.get(k) {
        Some(Value::String(x)) if !x.is_empty() => Some(x.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

fn list<T>(v: &Value, k: &str, f: impl Fn(&Value) -> T) -> Vec<T> {
    v.get(k).and_then(|x| x.as_array()).map(|a| a.iter().map(f).collect()).unwrap_or_default()
}

/// Inserta en `body` sólo los campos con valor (`None` no se manda: la instancia lo
/// interpretaría como «borrar»). Para borrar un campo de verdad, usa `Some("")`.
fn put_opt(body: &mut Map<String, Value>, k: &str, v: &Option<String>) {
    if let Some(x) = v {
        body.insert(k.to_string(), json!(x));
    }
}

/// `true` si el plugin está habilitado y accesible para el usuario (`GET /api/plugins`).
/// Ids: `iur-crm`, `iur-tickets`, `iur-collab`, `iur-portal`…
pub async fn plugin_available(sess: &Session, plugin_id: &str) -> Result<bool> {
    let v = sess.get_json("/api/plugins").await?;
    Ok(v["plugins"].as_array().map(|a| a.iter().any(|p| s(p, "id") == plugin_id)).unwrap_or(false))
}

// ---------------------------------------------------------------------------
// Agenda (eventos del calendario personal)
// ---------------------------------------------------------------------------

/// Evento del calendario personal (`/api/calendar-events`). Cada usuario ve sólo los suyos.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// ISO 8601 en UTC con `Z`.
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub location: Option<String>,
    pub meeting_url: Option<String>,
    /// meeting | appointment | reminder | personal | court | deadline | other
    pub event_type: String,
    pub color: Option<String>,
    pub case_id: Option<String>,
    pub case_title: Option<String>,
    /// Regla RRULE (RFC 5545) si el evento se repite.
    pub recurrence_rule: Option<String>,
    pub reminder_minutes: Option<i64>,
    /// `google` | `microsoft` si vino de una integración de la instancia.
    pub external_provider: Option<String>,
    pub updated_at: Option<String>,
}

pub(crate) fn event_from(v: &Value) -> CalendarEvent {
    CalendarEvent {
        id: id_of(v, "id").unwrap_or_default(),
        title: s(v, "title"),
        description: opt(v, "description"),
        start: s(v, "start_datetime"),
        end: s(v, "end_datetime"),
        all_day: b(v, "all_day"),
        location: opt(v, "location"),
        meeting_url: opt(v, "meeting_url"),
        event_type: opt(v, "event_type").unwrap_or_else(|| "other".into()),
        color: opt(v, "color"),
        case_id: opt(v, "case_id"),
        case_title: v.get("case").and_then(|c| opt(c, "title")),
        recurrence_rule: opt(v, "recurrence_rule"),
        reminder_minutes: v["reminder_minutes"].as_i64(),
        external_provider: opt(v, "external_provider"),
        updated_at: opt(v, "updated_at"),
    }
}

/// `GET /api/calendar-events?start=&end=&case_id=`: eventos que se cruzan con el rango.
/// `start`/`end` en ISO 8601 (con zona: la instancia los convierte a UTC).
pub async fn events(
    sess: &Session,
    start: Option<&str>,
    end: Option<&str>,
    case_id: Option<&str>,
) -> Result<Vec<CalendarEvent>> {
    let mut q = vec![];
    if let Some(x) = start {
        q.push(format!("start={}", urlencode(x)));
    }
    if let Some(x) = end {
        q.push(format!("end={}", urlencode(x)));
    }
    if let Some(x) = case_id {
        q.push(format!("case_id={}", urlencode(x)));
    }
    let path =
        if q.is_empty() { "/api/calendar-events".to_string() } else { format!("/api/calendar-events?{}", q.join("&")) };
    let v = sess.get_json(&path).await?;
    Ok(list(&v, "events", event_from))
}

/// `GET /api/calendar-events/<id>`.
pub async fn event(sess: &Session, id: &str) -> Result<CalendarEvent> {
    let v = sess.get_json(&format!("/api/calendar-events/{}", urlencode(id))).await?;
    Ok(event_from(&v["event"]))
}

/// Datos para crear o modificar un evento. Al modificar, sólo se mandan los campos `Some`.
#[derive(Debug, Clone, Default)]
pub struct EventInput {
    pub title: Option<String>,
    pub description: Option<String>,
    /// ISO 8601; con zona se convierte a UTC, sin zona se toma como UTC.
    pub start: Option<String>,
    pub end: Option<String>,
    pub all_day: Option<bool>,
    pub location: Option<String>,
    pub event_type: Option<String>,
    pub color: Option<String>,
    /// `Some("")` desvincula el evento del caso.
    pub case_id: Option<String>,
    pub reminder_minutes: Option<i64>,
}

fn event_body(e: &EventInput) -> Value {
    let mut m = Map::new();
    put_opt(&mut m, "title", &e.title);
    put_opt(&mut m, "description", &e.description);
    put_opt(&mut m, "start_datetime", &e.start);
    put_opt(&mut m, "end_datetime", &e.end);
    if let Some(x) = e.all_day {
        m.insert("all_day".into(), json!(x));
    }
    put_opt(&mut m, "location", &e.location);
    put_opt(&mut m, "event_type", &e.event_type);
    put_opt(&mut m, "color", &e.color);
    if let Some(c) = &e.case_id {
        m.insert("case_id".into(), if c.is_empty() { Value::Null } else { json!(c) });
    }
    if let Some(x) = e.reminder_minutes {
        m.insert("reminder_minutes".into(), json!(x));
    }
    Value::Object(m)
}

/// `POST /api/calendar-events` (título, inicio y fin obligatorios).
pub async fn create_event(sess: &Session, e: &EventInput) -> Result<CalendarEvent> {
    if e.title.as_deref().map_or(true, |t| t.trim().is_empty()) || e.start.is_none() || e.end.is_none() {
        return Err(anyhow!(lang::pick(
            "An event needs a title, a start and an end",
            "Un evento necesita título, inicio y fin"
        )));
    }
    let v = sess.post_json("/api/calendar-events", &event_body(e)).await?;
    Ok(event_from(&v["event"]))
}

/// `PUT /api/calendar-events/<id>` (sólo los campos `Some`).
pub async fn update_event(sess: &Session, id: &str, e: &EventInput) -> Result<CalendarEvent> {
    let v = sess.put_json(&format!("/api/calendar-events/{}", urlencode(id)), &event_body(e)).await?;
    Ok(event_from(&v["event"]))
}

/// `DELETE /api/calendar-events/<id>`.
pub async fn delete_event(sess: &Session, id: &str) -> Result<()> {
    sess.delete_json(&format!("/api/calendar-events/{}", urlencode(id))).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Contactos y clientes
// ---------------------------------------------------------------------------

/// Contacto del núcleo (`/api/contacts`): prospectos, testigos, peritos, proveedores…
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    pub id: String,
    pub display_name: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company: Option<String>,
    pub title: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub phone_alternate: Option<String>,
    pub address_line1: Option<String>,
    pub address_line2: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub notes: Option<String>,
    pub tags: Vec<String>,
    /// prospect | lead | witness | opposing_counsel | expert | court_staff | vendor | insurance | other
    pub contact_type: String,
    /// individual | company
    pub entity_type: Option<String>,
    pub parent_client_id: Option<String>,
    pub updated_at: Option<String>,
}

pub(crate) fn contact_from(v: &Value) -> Contact {
    Contact {
        id: id_of(v, "id").unwrap_or_default(),
        display_name: s(v, "display_name"),
        first_name: opt(v, "first_name"),
        last_name: opt(v, "last_name"),
        company: opt(v, "company"),
        title: opt(v, "title"),
        email: opt(v, "email"),
        phone: opt(v, "phone"),
        phone_alternate: opt(v, "phone_alternate"),
        address_line1: opt(v, "address_line1"),
        address_line2: opt(v, "address_line2"),
        city: opt(v, "city"),
        state: opt(v, "state"),
        postal_code: opt(v, "postal_code"),
        country: opt(v, "country"),
        notes: opt(v, "notes"),
        tags: strings(v, "tags"),
        contact_type: opt(v, "contact_type").unwrap_or_else(|| "other".into()),
        entity_type: opt(v, "entity_type"),
        parent_client_id: opt(v, "parent_client_id"),
        updated_at: opt(v, "updated_at"),
    }
}

/// `GET /api/contacts?type=&include_counts=false`: todos los contactos visibles (la
/// instancia no pagina ni busca aquí; filtra en local).
pub async fn contacts(sess: &Session, contact_type: Option<&str>) -> Result<Vec<Contact>> {
    let mut path = "/api/contacts?include_counts=false".to_string();
    if let Some(t) = contact_type {
        path.push_str(&format!("&type={}", urlencode(t)));
    }
    let v = sess.get_json(&path).await?;
    Ok(list(&v, "contacts", contact_from))
}

/// `GET /api/contacts/<id>`.
pub async fn contact(sess: &Session, id: &str) -> Result<Contact> {
    let v = sess.get_json(&format!("/api/contacts/{}", urlencode(id))).await?;
    Ok(contact_from(v.get("contact").unwrap_or(&v)))
}

/// Datos para crear o modificar un contacto (sólo se mandan los `Some`).
#[derive(Debug, Clone, Default)]
pub struct ContactInput {
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company: Option<String>,
    pub title: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub phone_alternate: Option<String>,
    pub address_line1: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    pub notes: Option<String>,
    pub contact_type: Option<String>,
    pub entity_type: Option<String>,
    pub parent_client_id: Option<String>,
}

fn contact_body(c: &ContactInput) -> Value {
    let mut m = Map::new();
    put_opt(&mut m, "first_name", &c.first_name);
    put_opt(&mut m, "last_name", &c.last_name);
    put_opt(&mut m, "company", &c.company);
    put_opt(&mut m, "title", &c.title);
    put_opt(&mut m, "email", &c.email);
    put_opt(&mut m, "phone", &c.phone);
    put_opt(&mut m, "phone_alternate", &c.phone_alternate);
    put_opt(&mut m, "address_line1", &c.address_line1);
    put_opt(&mut m, "city", &c.city);
    put_opt(&mut m, "state", &c.state);
    put_opt(&mut m, "postal_code", &c.postal_code);
    put_opt(&mut m, "country", &c.country);
    put_opt(&mut m, "notes", &c.notes);
    put_opt(&mut m, "contact_type", &c.contact_type);
    put_opt(&mut m, "entity_type", &c.entity_type);
    put_opt(&mut m, "parent_client_id", &c.parent_client_id);
    Value::Object(m)
}

/// La instancia contesta 409 si ya hay un contacto con ese correo.
fn map_duplicate(e: anyhow::Error) -> anyhow::Error {
    if e.to_string().starts_with("409") {
        anyhow!(lang::pick(
            "A record with that email already exists on the instance",
            "Ya existe en la instancia un registro con ese correo"
        ))
    } else {
        e
    }
}

/// `POST /api/contacts` (`contact_type` `other` por omisión).
pub async fn create_contact(sess: &Session, c: &ContactInput) -> Result<Contact> {
    let v = sess.post_json("/api/contacts", &contact_body(c)).await.map_err(map_duplicate)?;
    Ok(contact_from(v.get("contact").unwrap_or(&v)))
}

/// `PUT /api/contacts/<id>` (sólo los campos `Some`).
pub async fn update_contact(sess: &Session, id: &str, c: &ContactInput) -> Result<Contact> {
    let v = sess.put_json(&format!("/api/contacts/{}", urlencode(id)), &contact_body(c)).await?;
    Ok(contact_from(v.get("contact").unwrap_or(&v)))
}

/// Cliente del núcleo (`/api/clients`), persona física o moral.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Client {
    pub id: String,
    pub display_name: String,
    /// individual | company
    pub client_type: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub tax_id: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub mobile: Option<String>,
    pub address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub postal_code: Option<String>,
    pub country: Option<String>,
    /// customer | supplier | both
    pub relationship_type: Option<String>,
    pub updated_at: Option<String>,
}

pub(crate) fn client_from(v: &Value) -> Client {
    Client {
        id: id_of(v, "id").unwrap_or_default(),
        display_name: s(v, "display_name"),
        client_type: opt(v, "client_type"),
        first_name: opt(v, "first_name"),
        last_name: opt(v, "last_name"),
        company_name: opt(v, "company_name"),
        tax_id: opt(v, "tax_id"),
        email: opt(v, "email"),
        phone: opt(v, "phone"),
        mobile: opt(v, "mobile"),
        address: opt(v, "address"),
        city: opt(v, "city"),
        state: opt(v, "state"),
        postal_code: opt(v, "postal_code"),
        country: opt(v, "country"),
        relationship_type: opt(v, "relationship_type"),
        updated_at: opt(v, "updated_at"),
    }
}

/// Una página de resultados.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub has_next: bool,
}

fn pagination(v: &Value, shown: usize) -> (u64, bool) {
    let p = &v["pagination"];
    let total = p["total"].as_u64().unwrap_or(shown as u64);
    (total, b(p, "has_next"))
}

/// `GET /api/clients?include_counts=false&paginate=true&page=&limit=` (clientes activos;
/// `limit` máx. 200). Para recorrerlos todos, pide páginas mientras `has_next`.
pub async fn clients(sess: &Session, page: u32, limit: u32) -> Result<Page<Client>> {
    let path =
        format!("/api/clients?include_counts=false&paginate=true&page={}&limit={}", page.max(1), limit.clamp(1, 200));
    let v = sess.get_json(&path).await?;
    let items = list(&v, "clients", client_from);
    let (total, has_next) = pagination(&v, items.len());
    Ok(Page { items, total, has_next })
}

/// `GET /api/clients/<id>`.
pub async fn client(sess: &Session, id: &str) -> Result<Client> {
    let v = sess.get_json(&format!("/api/clients/{}", urlencode(id))).await?;
    Ok(client_from(v.get("client").unwrap_or(&v)))
}

// ---------------------------------------------------------------------------
// Búsqueda global (para vincular un correo con un caso, cliente, tarea…)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    /// case | client | document | task | o la clave del grupo de un plugin (`leads`…).
    pub kind: String,
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
}

pub(crate) fn search_hits(v: &Value) -> Vec<SearchHit> {
    let r = &v["results"];
    let mut out = vec![];
    for c in r["cases"].as_array().into_iter().flatten() {
        let n = s(c, "case_number");
        let t = s(c, "title");
        out.push(SearchHit {
            kind: "case".into(),
            id: id_of(c, "id").unwrap_or_default(),
            title: if n.is_empty() { t } else { format!("{n} · {t}") },
            subtitle: opt(c, "client"),
        });
    }
    for c in r["clients"].as_array().into_iter().flatten() {
        out.push(SearchHit {
            kind: "client".into(),
            id: id_of(c, "id").unwrap_or_default(),
            title: s(c, "display_name"),
            subtitle: opt(c, "email"),
        });
    }
    for d in r["documents"].as_array().into_iter().flatten() {
        out.push(SearchHit {
            kind: "document".into(),
            id: id_of(d, "id").unwrap_or_default(),
            title: s(d, "title"),
            subtitle: opt(d, "file_name"),
        });
    }
    for t in r["tasks"].as_array().into_iter().flatten() {
        out.push(SearchHit {
            kind: "task".into(),
            id: id_of(t, "id").unwrap_or_default(),
            title: s(t, "title"),
            subtitle: opt(t, "due_date"),
        });
    }
    for g in r["plugins"].as_array().into_iter().flatten() {
        let key = s(g, "key");
        for i in g["items"].as_array().into_iter().flatten() {
            out.push(SearchHit {
                kind: key.clone(),
                id: id_of(i, "id").unwrap_or_default(),
                title: s(i, "title"),
                subtitle: opt(i, "subtitle"),
            });
        }
    }
    out
}

/// `GET /api/search?q=`: casos, clientes, documentos, tareas y grupos de plugins (CRM…),
/// hasta 10 por grupo. Menos de 2 caracteres → lista vacía.
pub async fn search(sess: &Session, q: &str) -> Result<Vec<SearchHit>> {
    let q = q.trim();
    if q.chars().count() < 2 {
        return Ok(vec![]);
    }
    let v = sess.get_json(&format!("/api/search?q={}", urlencode(q))).await?;
    Ok(search_hits(&v))
}

// ---------------------------------------------------------------------------
// Tareas («mis tareas», incluidas las personales sin caso)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    /// pending | in_progress | completed | cancelled (u otros configurados en la instancia)
    pub status: String,
    /// low | medium | high | urgent
    pub priority: Option<String>,
    pub case_id: Option<String>,
    pub case_title: Option<String>,
    pub assigned_to_id: Option<String>,
    /// Fecha de pared (sin zona).
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub completed_at: Option<String>,
    pub is_overdue: bool,
    pub recurrence_rule: Option<String>,
    pub updated_at: Option<String>,
}

pub(crate) fn task_from(v: &Value) -> Task {
    Task {
        id: id_of(v, "id").unwrap_or_default(),
        title: s(v, "title"),
        description: opt(v, "description"),
        status: s(v, "status"),
        priority: opt(v, "priority"),
        case_id: opt(v, "case_id"),
        case_title: v.get("case").and_then(|c| opt(c, "title")).or_else(|| opt(v, "case_title")),
        assigned_to_id: id_of(v, "assigned_to_id"),
        start_date: opt(v, "start_date"),
        due_date: opt(v, "due_date"),
        completed_at: opt(v, "completed_at"),
        is_overdue: b(v, "is_overdue"),
        recurrence_rule: opt(v, "recurrence_rule"),
        updated_at: opt(v, "updated_at"),
    }
}

/// `GET /api/tasks?assigned_to=<user>&status=…&include_case=true`: tareas donde el usuario
/// es responsable o corresponsable (con o sin caso), fuera de casos archivados.
/// `user_id` es el `id` de [`crate::rest::User`]; `statuses` vacío = todas.
pub async fn tasks_assigned_to(
    sess: &Session,
    user_id: &str,
    statuses: &[&str],
    page: u32,
    limit: u32,
) -> Result<Page<Task>> {
    let mut path = format!(
        "/api/tasks?assigned_to={}&include_case=true&paginate=true&page={}&limit={}",
        urlencode(user_id),
        page.max(1),
        limit.clamp(1, 500)
    );
    if !statuses.is_empty() {
        path.push_str(&format!("&status={}", urlencode(&statuses.join(","))));
    }
    let v = sess.get_json(&path).await?;
    let items = list(&v, "tasks", task_from);
    let (total, has_next) = pagination(&v, items.len());
    Ok(Page { items, total, has_next })
}

/// Atajo: tareas abiertas (`pending`, `in_progress`) del usuario.
pub async fn my_open_tasks(sess: &Session, user_id: &str, limit: u32) -> Result<Vec<Task>> {
    Ok(tasks_assigned_to(sess, user_id, &["pending", "in_progress"], 1, limit).await?.items)
}

/// `GET /api/tasks/<id>`.
pub async fn task(sess: &Session, id: &str) -> Result<Task> {
    let v = sess.get_json(&format!("/api/tasks/{}", urlencode(id))).await?;
    Ok(task_from(&v["task"]))
}

/// Datos para crear o modificar una tarea (sólo se mandan los `Some`).
#[derive(Debug, Clone, Default)]
pub struct TaskInput {
    pub title: Option<String>,
    pub description: Option<String>,
    /// Sin caso → tarea personal.
    pub case_id: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub assigned_to_id: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub location: Option<String>,
}

fn task_body(t: &TaskInput) -> Value {
    let mut m = Map::new();
    put_opt(&mut m, "title", &t.title);
    put_opt(&mut m, "description", &t.description);
    put_opt(&mut m, "case_id", &t.case_id);
    put_opt(&mut m, "status", &t.status);
    put_opt(&mut m, "priority", &t.priority);
    put_opt(&mut m, "assigned_to_id", &t.assigned_to_id);
    put_opt(&mut m, "start_date", &t.start_date);
    put_opt(&mut m, "due_date", &t.due_date);
    put_opt(&mut m, "location", &t.location);
    Value::Object(m)
}

/// `POST /api/tasks` (título obligatorio). Para que sea «mía», pon `assigned_to_id`.
pub async fn create_task(sess: &Session, t: &TaskInput) -> Result<Task> {
    if t.title.as_deref().map_or(true, |x| x.trim().is_empty()) {
        return Err(anyhow!(lang::pick("A task needs a title", "Una tarea necesita título")));
    }
    let v = sess.post_json("/api/tasks", &task_body(t)).await?;
    Ok(task_from(v.get("task").unwrap_or(&v)))
}

/// `PUT /api/tasks/<id>` (sólo los campos `Some`).
pub async fn update_task(sess: &Session, id: &str, t: &TaskInput) -> Result<Task> {
    let v = sess.put_json(&format!("/api/tasks/{}", urlencode(id)), &task_body(t)).await?;
    Ok(task_from(v.get("task").unwrap_or(&v)))
}

/// Marca una tarea como completada (o la reabre como `pending`).
pub async fn set_task_done(sess: &Session, id: &str, done: bool) -> Result<Task> {
    let status = if done { "completed" } else { "pending" };
    update_task(sess, id, &TaskInput { status: Some(status.into()), ..Default::default() }).await
}

// ---------------------------------------------------------------------------
// Avisos (notificaciones). No hay canal en vivo para escritorio: se consulta periódicamente.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub id: String,
    /// task_assigned, task_due_soon, deadline_approaching, mention…
    pub kind: String,
    pub title: String,
    pub message: Option<String>,
    pub related_type: Option<String>,
    pub related_id: Option<String>,
    /// Ruta relativa en la aplicación web (p. ej. `/dashboard/cases/…`).
    pub link: Option<String>,
    pub is_read: bool,
    pub created_at: Option<String>,
}

pub(crate) fn notification_from(v: &Value) -> Notification {
    Notification {
        id: id_of(v, "id").unwrap_or_default(),
        kind: s(v, "type"),
        title: s(v, "title"),
        message: opt(v, "message"),
        related_type: opt(v, "related_type"),
        related_id: id_of(v, "related_id"),
        link: opt(v, "link"),
        is_read: b(v, "is_read") || b(v, "read"),
        created_at: opt(v, "created_at"),
    }
}

/// `GET /api/notifications?unread_only=&limit=`. Devuelve los avisos y el total sin leer.
pub async fn notifications(sess: &Session, unread_only: bool, limit: u32) -> Result<(Vec<Notification>, u64)> {
    let v =
        sess.get_json(&format!("/api/notifications?unread_only={unread_only}&limit={}", limit.clamp(1, 200))).await?;
    Ok((list(&v, "notifications", notification_from), v["unread_count"].as_u64().unwrap_or(0)))
}

/// `PUT /api/notifications/<id>/read`.
pub async fn mark_notification_read(sess: &Session, id: &str) -> Result<()> {
    sess.put_json(&format!("/api/notifications/{}/read", urlencode(id)), &json!({})).await?;
    Ok(())
}

/// `PUT /api/notifications/mark-all-read`.
pub async fn mark_all_notifications_read(sess: &Session) -> Result<()> {
    sess.put_json("/api/notifications/mark-all-read", &json!({})).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Colaboración (plugin iur-collab): comentarios y menciones
// ---------------------------------------------------------------------------

/// Entidades que admiten comentarios: task, calendar_event, deadline, case, client, contact,
/// document, lead, opportunity, quote, invoice… (no tickets: tienen sus propios comentarios).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub target_type: String,
    pub target_id: String,
    pub parent_id: Option<String>,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    /// `None` si se borró.
    pub content: Option<String>,
    /// markdown | text
    pub content_format: Option<String>,
    pub reply_count: u64,
    pub created_at: Option<String>,
}

pub(crate) fn comment_from(v: &Value) -> Comment {
    Comment {
        id: id_of(v, "id").unwrap_or_default(),
        target_type: s(v, "target_type"),
        target_id: id_of(v, "target_id").unwrap_or_default(),
        parent_id: opt(v, "parent_id"),
        user_id: id_of(v, "user_id"),
        user_name: opt(v, "user_name"),
        content: opt(v, "content"),
        content_format: opt(v, "content_format"),
        reply_count: v["reply_count"].as_u64().unwrap_or(0),
        created_at: opt(v, "created_at"),
    }
}

/// `GET /api/plugins/collab/entities/<tipo>/<id>/comments?page=&per_page=` (más viejos primero).
pub async fn comments(sess: &Session, target_type: &str, target_id: &str, page: u32) -> Result<Page<Comment>> {
    let v = sess
        .get_json(&format!(
            "/api/plugins/collab/entities/{}/{}/comments?page={}&per_page=100",
            urlencode(target_type),
            urlencode(target_id),
            page.max(1)
        ))
        .await?;
    let items = list(&v, "comments", comment_from);
    let p = &v["pagination"];
    let total = p["total"].as_u64().unwrap_or(items.len() as u64);
    let has_next = p["page"].as_u64().unwrap_or(1) < p["total_pages"].as_u64().unwrap_or(1);
    Ok(Page { items, total, has_next })
}

/// `POST /api/plugins/collab/entities/<tipo>/<id>/comments` (Markdown; `@` para mencionar).
pub async fn add_comment(
    sess: &Session,
    target_type: &str,
    target_id: &str,
    content: &str,
    parent_id: Option<&str>,
) -> Result<Comment> {
    let mut body = json!({"content": content, "content_format": "markdown"});
    if let Some(p) = parent_id {
        body["parent_id"] = json!(p);
    }
    let v = sess
        .post_json(
            &format!("/api/plugins/collab/entities/{}/{}/comments", urlencode(target_type), urlencode(target_id)),
            &body,
        )
        .await?;
    Ok(comment_from(&v["comment"]))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Mention {
    pub id: String,
    pub comment_id: Option<String>,
    pub mentioned_by_name: Option<String>,
    pub target_type: String,
    pub target_id: String,
    pub is_read: bool,
    pub created_at: Option<String>,
}

pub(crate) fn mention_from(v: &Value) -> Mention {
    Mention {
        id: id_of(v, "id").unwrap_or_default(),
        comment_id: opt(v, "comment_id"),
        mentioned_by_name: opt(v, "mentioned_by_name"),
        target_type: s(v, "target_type"),
        target_id: id_of(v, "target_id").unwrap_or_default(),
        is_read: b(v, "is_read"),
        created_at: opt(v, "created_at"),
    }
}

/// `GET /api/plugins/collab/mentions?unread=&limit=`: dónde me mencionaron.
pub async fn mentions(sess: &Session, unread_only: bool, limit: u32) -> Result<Vec<Mention>> {
    let v = sess
        .get_json(&format!("/api/plugins/collab/mentions?unread={unread_only}&limit={}", limit.clamp(1, 200)))
        .await?;
    Ok(list(&v, "mentions", mention_from))
}

/// `POST /api/plugins/collab/mentions/<id>/read`.
pub async fn mark_mention_read(sess: &Session, id: &str) -> Result<()> {
    sess.post_json(&format!("/api/plugins/collab/mentions/{}/read", urlencode(id)), &json!({})).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tickets (plugin iur-tickets)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TicketComment {
    pub id: String,
    pub content: String,
    pub is_internal: bool,
    /// manual | email | system
    pub source: Option<String>,
    pub author: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Ticket {
    pub id: String,
    pub ticket_number: String,
    pub title: String,
    pub description: Option<String>,
    /// open | in_progress | waiting | resolved | closed
    pub status: String,
    /// low | medium | high | urgent
    pub priority: String,
    pub queue_id: Option<String>,
    pub queue_name: Option<String>,
    pub case_id: Option<String>,
    pub case_title: Option<String>,
    pub client_id: Option<String>,
    pub client_name: Option<String>,
    pub assigned_to: Option<String>,
    pub assignee_name: Option<String>,
    pub requester_name: Option<String>,
    pub requester_email: Option<String>,
    /// manual | email | api | catalog
    pub source: Option<String>,
    pub sla_deadline: Option<String>,
    pub sla_breached: bool,
    pub tags: Vec<String>,
    pub updated_at: Option<String>,
    /// Sólo en [`ticket`] (detalle).
    pub comments: Vec<TicketComment>,
}

pub(crate) fn ticket_from(v: &Value) -> Ticket {
    Ticket {
        id: id_of(v, "id").unwrap_or_default(),
        ticket_number: s(v, "ticket_number"),
        title: s(v, "title"),
        description: opt(v, "description"),
        status: s(v, "status"),
        priority: s(v, "priority"),
        queue_id: opt(v, "queue_id"),
        queue_name: opt(v, "queue_name"),
        case_id: opt(v, "case_id"),
        case_title: opt(v, "case_title"),
        client_id: opt(v, "client_id"),
        client_name: opt(v, "client_name"),
        assigned_to: id_of(v, "assigned_to"),
        assignee_name: opt(v, "assignee_name"),
        requester_name: opt(v, "requester_name"),
        requester_email: opt(v, "requester_email"),
        source: opt(v, "source"),
        sla_deadline: opt(v, "sla_deadline"),
        sla_breached: b(v, "sla_breached"),
        tags: strings(v, "tags"),
        updated_at: opt(v, "updated_at"),
        comments: list(v, "comments", |c| TicketComment {
            id: id_of(c, "id").unwrap_or_default(),
            content: s(c, "content"),
            is_internal: b(c, "is_internal"),
            source: opt(c, "source"),
            author: opt(c, "user_name").or_else(|| opt(c, "sender_name")).or_else(|| opt(c, "sender_email")),
            created_at: opt(c, "created_at"),
        }),
    }
}

/// Filtros de [`tickets`].
#[derive(Debug, Clone, Default)]
pub struct TicketQuery {
    /// `active` (open + in_progress + waiting), o un estado concreto.
    pub status: Option<String>,
    pub search: Option<String>,
    pub case_id: Option<String>,
    pub assigned_to: Option<String>,
    pub reported_by_me: bool,
    pub page: u32,
    pub per_page: u32,
}

/// `GET /api/plugins/tickets?…` (más recientes primero).
pub async fn tickets(sess: &Session, q: &TicketQuery) -> Result<Page<Ticket>> {
    let mut path = format!("/api/plugins/tickets?page={}&per_page={}", q.page.max(1), q.per_page.clamp(1, 100));
    for (k, v) in
        [("status", &q.status), ("search", &q.search), ("case_id", &q.case_id), ("assigned_to", &q.assigned_to)]
    {
        if let Some(x) = v.as_deref().map(str::trim).filter(|x| !x.is_empty()) {
            path.push_str(&format!("&{k}={}", urlencode(x)));
        }
    }
    if q.reported_by_me {
        path.push_str("&reported_by_me=true");
    }
    let v = sess.get_json(&path).await?;
    let items = list(&v, "tickets", ticket_from);
    let (total, has_next) = pagination(&v, items.len());
    Ok(Page { items, total, has_next })
}

/// `GET /api/plugins/tickets/<id>` (con comentarios).
pub async fn ticket(sess: &Session, id: &str) -> Result<Ticket> {
    let v = sess.get_json(&format!("/api/plugins/tickets/{}", urlencode(id))).await?;
    Ok(ticket_from(&v["ticket"]))
}

/// Datos para crear un ticket.
#[derive(Debug, Clone, Default)]
pub struct NewTicket {
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<String>,
    /// Sin cola → la cola por omisión de la instancia.
    pub queue_id: Option<String>,
    pub case_id: Option<String>,
    pub client_id: Option<String>,
    pub category: Option<String>,
    pub requester_name: Option<String>,
    /// OJO: si se pone, la instancia contesta por correo al solicitante cada comentario
    /// público. Desde IureDesk el usuario contesta con su propio correo: déjalo en `None`
    /// salvo que se quiera ese comportamiento.
    pub requester_email: Option<String>,
    pub tags: Vec<String>,
}

/// `POST /api/plugins/tickets` (con `source: "api"`).
pub async fn create_ticket(sess: &Session, t: &NewTicket) -> Result<Ticket> {
    if t.title.trim().is_empty() {
        return Err(anyhow!(lang::pick("A ticket needs a title", "Un ticket necesita título")));
    }
    let mut m = Map::new();
    m.insert("title".into(), json!(t.title));
    m.insert("source".into(), json!("api"));
    put_opt(&mut m, "description", &t.description);
    put_opt(&mut m, "priority", &t.priority);
    put_opt(&mut m, "queue_id", &t.queue_id);
    put_opt(&mut m, "case_id", &t.case_id);
    put_opt(&mut m, "client_id", &t.client_id);
    put_opt(&mut m, "category", &t.category);
    put_opt(&mut m, "requester_name", &t.requester_name);
    put_opt(&mut m, "requester_email", &t.requester_email);
    if !t.tags.is_empty() {
        m.insert("tags".into(), json!(t.tags));
    }
    let v = sess.post_json("/api/plugins/tickets", &Value::Object(m)).await?;
    Ok(ticket_from(&v["ticket"]))
}

/// `PUT /api/plugins/tickets/<id>` con un cambio de estado (`resolved`, `waiting`…).
/// Los tickets tipificados pueden rechazar la transición: el error trae el motivo.
pub async fn set_ticket_status(sess: &Session, id: &str, status: &str) -> Result<Ticket> {
    let v = sess.put_json(&format!("/api/plugins/tickets/{}", urlencode(id)), &json!({"status": status})).await?;
    Ok(ticket_from(&v["ticket"]))
}

/// `POST /api/plugins/tickets/<id>/comments`. `internal = true` → nota interna (el
/// solicitante no la ve). Un comentario público reabre un ticket resuelto.
pub async fn ticket_comment(sess: &Session, id: &str, content: &str, internal: bool) -> Result<TicketComment> {
    let v = sess
        .post_json(
            &format!("/api/plugins/tickets/{}/comments", urlencode(id)),
            &json!({"content": content, "is_internal": internal}),
        )
        .await?;
    let c = &v["comment"];
    Ok(TicketComment {
        id: id_of(c, "id").unwrap_or_default(),
        content: s(c, "content"),
        is_internal: b(c, "is_internal"),
        source: opt(c, "source"),
        author: opt(c, "user_name"),
        created_at: opt(c, "created_at"),
    })
}

/// `POST /api/plugins/tickets/<id>/documents`: vincula un documento ya subido
/// (p. ej. el correo exportado con [`crate::api::upload_document`]).
pub async fn ticket_attach_document(sess: &Session, id: &str, document_id: &str, notes: Option<&str>) -> Result<()> {
    let mut body = json!({"document_id": document_id});
    if let Some(n) = notes {
        body["notes"] = json!(n);
    }
    sess.post_json(&format!("/api/plugins/tickets/{}/documents", urlencode(id)), &body).await.map_err(|e| {
        if e.to_string().starts_with("409") {
            anyhow!(lang::pick(
                "That document is already linked to the ticket",
                "Ese documento ya está vinculado al ticket"
            ))
        } else {
            e
        }
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CRM (plugin iur-crm): personas, actividades y correo de la venta
// ---------------------------------------------------------------------------

/// Lead del CRM con sus datos de contacto (para el directorio de contactos).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrmPerson {
    pub id: String,
    /// person | company
    pub lead_type: String,
    pub name: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub organization_id: Option<String>,
    pub organization_name: Option<String>,
    pub position: Option<String>,
    pub emails: Vec<String>,
    pub phones: Vec<String>,
    pub address: Option<String>,
    pub location: Option<String>,
    pub url: Option<String>,
    /// active | converted | archived
    pub status: String,
    pub do_not_contact: bool,
    pub tags: Vec<String>,
    pub updated_at: Option<String>,
}

pub(crate) fn crm_person_from(v: &Value) -> CrmPerson {
    let nonempty =
        |ks: &[&str]| ks.iter().filter_map(|k| opt(v, k)).filter(|x| !x.trim().is_empty()).collect::<Vec<_>>();
    CrmPerson {
        id: id_of(v, "id").unwrap_or_default(),
        lead_type: opt(v, "lead_type").unwrap_or_else(|| "person".into()),
        name: s(v, "name"),
        first_name: opt(v, "first_name"),
        last_name: opt(v, "last_name"),
        // `company` en los candidatos de `/extract`.
        company_name: opt(v, "company_name").or_else(|| opt(v, "company")),
        organization_id: opt(v, "organization_id"),
        organization_name: opt(v, "organization_name"),
        position: opt(v, "position"),
        emails: nonempty(&["email", "email2", "email3"]),
        phones: nonempty(&["phone", "phone2", "phone3"]),
        address: opt(v, "address"),
        location: opt(v, "location"),
        url: opt(v, "url"),
        status: s(v, "status"),
        do_not_contact: b(v, "do_not_contact"),
        tags: strings(v, "tags"),
        updated_at: opt(v, "updated_at"),
    }
}

/// `GET /api/plugins/crm/leads?search=&limit=&offset=` con todos sus datos de contacto
/// (`limit` máx. 500). `search` busca en nombre, empresa y correo.
pub async fn crm_people(sess: &Session, search: Option<&str>, limit: u32, offset: u32) -> Result<Page<CrmPerson>> {
    let limit = limit.clamp(1, 500);
    let mut path = format!("/api/plugins/crm/leads?limit={limit}&offset={offset}");
    if let Some(q) = search.map(str::trim).filter(|q| !q.is_empty()) {
        path.push_str(&format!("&search={}", urlencode(q)));
    }
    let v = sess.get_json(&path).await?;
    let items = list(&v, "items", crm_person_from);
    let total = v["total"].as_u64().unwrap_or(items.len() as u64);
    let has_next = (offset as u64).saturating_add(items.len() as u64) < total;
    Ok(Page { items, total, has_next })
}

/// Datos para crear un lead desde un correo.
#[derive(Debug, Clone, Default)]
pub struct NewLead {
    pub name: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub position: Option<String>,
    pub notes: Option<String>,
}

/// `POST /api/plugins/crm/leads` (`source: "manual"`). 409 si ya hay un lead con ese correo.
pub async fn crm_create_lead(sess: &Session, l: &NewLead) -> Result<CrmPerson> {
    if l.name.trim().is_empty() {
        return Err(anyhow!(lang::pick("A lead needs a name", "Un lead necesita nombre")));
    }
    let mut m = Map::new();
    m.insert("name".into(), json!(l.name));
    m.insert("source".into(), json!("manual"));
    put_opt(&mut m, "first_name", &l.first_name);
    put_opt(&mut m, "last_name", &l.last_name);
    put_opt(&mut m, "company_name", &l.company_name);
    put_opt(&mut m, "email", &l.email);
    put_opt(&mut m, "phone", &l.phone);
    put_opt(&mut m, "position", &l.position);
    put_opt(&mut m, "notes", &l.notes);
    let v = sess.post_json("/api/plugins/crm/leads", &Value::Object(m)).await.map_err(map_duplicate)?;
    Ok(crm_person_from(&v))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrmActivity {
    pub id: String,
    /// call | meeting | email | whatsapp | sms | note | task | osint | system
    pub activity_type: String,
    pub subject: Option<String>,
    pub description: Option<String>,
    pub scheduled_at: Option<String>,
    pub completed_at: Option<String>,
    pub created_at: Option<String>,
}

pub(crate) fn crm_activity_from(v: &Value) -> CrmActivity {
    CrmActivity {
        id: id_of(v, "id").unwrap_or_default(),
        activity_type: s(v, "activity_type"),
        subject: opt(v, "subject"),
        description: opt(v, "description"),
        scheduled_at: opt(v, "scheduled_at"),
        completed_at: opt(v, "completed_at"),
        created_at: opt(v, "created_at"),
    }
}

/// `GET /api/plugins/crm/activities?{lead_id|opportunity_id}=`: historia del lead u oportunidad.
pub async fn crm_activities(sess: &Session, kind: CrmKind, id: &str, limit: u32) -> Result<Vec<CrmActivity>> {
    let key = match kind {
        CrmKind::Lead => "lead_id",
        CrmKind::Opportunity => "opportunity_id",
    };
    let v = sess
        .get_json(&format!("/api/plugins/crm/activities?{key}={}&limit={}", urlencode(id), limit.clamp(1, 500)))
        .await?;
    Ok(list(&v, "items", crm_activity_from))
}

/// Correo guardado en la historia de una venta (buzón de ventas del CRM).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CrmEmail {
    pub id: String,
    /// Cabecera `Message-ID` (para no duplicar y para hilar).
    pub message_id: String,
    /// inbound | outbound
    pub direction: String,
    pub from_name: Option<String>,
    pub from_email: Option<String>,
    pub to_emails: Vec<String>,
    pub subject: Option<String>,
    pub email_date: Option<String>,
    pub lead_id: Option<String>,
    pub opportunity_id: Option<String>,
    /// token | thread | address | manual | none
    pub matched_by: Option<String>,
    pub body_text: Option<String>,
}

pub(crate) fn crm_email_from(v: &Value) -> CrmEmail {
    CrmEmail {
        id: id_of(v, "id").unwrap_or_default(),
        message_id: s(v, "message_id"),
        direction: s(v, "direction"),
        from_name: opt(v, "from_name"),
        from_email: opt(v, "from_email"),
        to_emails: strings(v, "to_emails"),
        subject: opt(v, "subject"),
        email_date: opt(v, "email_date"),
        lead_id: opt(v, "lead_id"),
        opportunity_id: opt(v, "opportunity_id"),
        matched_by: opt(v, "matched_by"),
        body_text: opt(v, "body_text"),
    }
}

/// `GET /api/plugins/crm/emails?{lead_id|opportunity_id}=`: correos ya archivados en la venta.
pub async fn crm_emails(sess: &Session, kind: CrmKind, id: &str, limit: u32) -> Result<Vec<CrmEmail>> {
    let key = match kind {
        CrmKind::Lead => "lead_id",
        CrmKind::Opportunity => "opportunity_id",
    };
    let v = sess
        .get_json(&format!("/api/plugins/crm/emails?{key}={}&limit={}", urlencode(id), limit.clamp(1, 200)))
        .await?;
    Ok(list(&v, "items", crm_email_from))
}

/// Direcciones del buzón de ventas para una oportunidad: si IureDesk pone `reply_to` como
/// «Responder a» y `bcc` en copia oculta, la instancia archiva el envío y la respuesta en la
/// oportunidad sin más pasos. `None` si el buzón no está configurado.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrmReplyAddress {
    pub reply_to: String,
    pub bcc: Option<String>,
}

/// `GET /api/plugins/crm/opportunities/<id>/reply-address`.
pub async fn crm_reply_address(sess: &Session, opportunity_id: &str) -> Result<Option<CrmReplyAddress>> {
    let v =
        sess.get_json(&format!("/api/plugins/crm/opportunities/{}/reply-address", urlencode(opportunity_id))).await?;
    if !b(&v, "configured") {
        return Ok(None);
    }
    Ok(opt(&v, "reply_to").filter(|r| !r.is_empty()).map(|reply_to| CrmReplyAddress { reply_to, bcc: opt(&v, "bcc") }))
}

// ---------------------------------------------------------------------------
// Ayudas de IA (consumen la cuota del plan)
// ---------------------------------------------------------------------------

fn map_ai_error(e: anyhow::Error) -> anyhow::Error {
    let m = e.to_string();
    if m.contains("quota_exceeded") || m.starts_with("429") {
        anyhow!(lang::pick(
            "The plan's AI quota on the instance has been reached",
            "Se alcanzó la cuota de IA del plan en la instancia"
        ))
    } else {
        e
    }
}

/// `POST /api/cases/<id>/ai-summary`: resumen del caso (tareas, plazos, avance, recomendaciones).
pub async fn case_ai_summary(sess: &Session, case_id: &str) -> Result<String> {
    let v = sess
        .post_json(&format!("/api/cases/{}/ai-summary", urlencode(case_id)), &json!({}))
        .await
        .map_err(map_ai_error)?;
    let t = s(&v, "summary").trim().to_string();
    if t.is_empty() {
        return Err(anyhow!(lang::pick("The instance returned no summary", "La instancia no devolvió resumen")));
    }
    Ok(t)
}

/// `POST /api/plugins/crm/extract` con `persist: false`: la instancia extrae leads (nombre,
/// empresa, correo, teléfono…) de un texto libre —p. ej. la firma de un correo— sin
/// guardarlos. El usuario confirma y luego se crean con [`crm_create_lead`].
pub async fn crm_extract_leads(sess: &Session, text: &str) -> Result<Vec<CrmPerson>> {
    if text.trim().is_empty() {
        return Ok(vec![]);
    }
    let v = sess
        .post_json("/api/plugins/crm/extract", &json!({"text": text, "persist": false}))
        .await
        .map_err(map_ai_error)?;
    Ok(list(&v, "preview", crm_person_from))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evento_de_la_instancia() {
        let v = json!({
            "id": "e1", "title": "Audiencia", "description": null,
            "start_datetime": "2026-10-27T16:00:00Z", "end_datetime": "2026-10-27T17:00:00Z",
            "all_day": false, "location": "Juzgado 3", "meeting_url": null, "event_type": "court",
            "case_id": "c1", "case": {"id": "c1", "title": "Pérez vs. Gómez"},
            "recurrence_rule": null, "reminder_minutes": 30, "external_provider": null
        });
        let e = event_from(&v);
        assert_eq!(e.start, "2026-10-27T16:00:00Z");
        assert_eq!(e.case_title.as_deref(), Some("Pérez vs. Gómez"));
        assert_eq!(e.reminder_minutes, Some(30));
        assert!(e.description.is_none() && !e.all_day);
    }

    #[test]
    fn evento_vacio_no_revienta() {
        let e = event_from(&Value::Null);
        assert!(e.id.is_empty());
        assert_eq!(e.event_type, "other");
    }

    #[test]
    fn cuerpo_de_evento_solo_lleva_lo_que_cambia() {
        let body =
            event_body(&EventInput { title: Some("x".into()), case_id: Some(String::new()), ..Default::default() });
        let m = body.as_object().unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m["case_id"], Value::Null);
    }

    #[test]
    fn contacto_y_cliente() {
        let c = contact_from(
            &json!({"id": "k1", "display_name": "Ana Ruiz", "email": "ana@x.mx", "tags": ["vip", 3], "contact_type": "expert"}),
        );
        assert_eq!(c.tags, ["vip"]);
        assert_eq!(c.contact_type, "expert");
        let k = client_from(
            &json!({"id": "c9", "display_name": "ACME SA", "client_type": "company", "tax_id": "ACM010101AAA"}),
        );
        assert_eq!(k.tax_id.as_deref(), Some("ACM010101AAA"));
    }

    #[test]
    fn paginacion() {
        let v = json!({"pagination": {"page": 1, "limit": 2, "total": 5, "total_pages": 3, "has_next": true}});
        assert_eq!(pagination(&v, 2), (5, true));
        assert_eq!(pagination(&json!({}), 3), (3, false));
    }

    #[test]
    fn busqueda_aplana_los_grupos() {
        let v = json!({"results": {
            "cases": [{"id": "c1", "case_number": "2026-01", "title": "Divorcio", "client": "Ana"}],
            "clients": [{"id": "k1", "display_name": "Ana", "email": "a@x.mx"}],
            "documents": [], "tasks": [],
            "plugins": [{"key": "leads", "label": "Leads", "items": [{"id": "l1", "title": "Beto"}]}]
        }});
        let h = search_hits(&v);
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].title, "2026-01 · Divorcio");
        assert_eq!(h[2].kind, "leads");
    }

    #[test]
    fn tarea_personal() {
        let t = task_from(
            &json!({"id": "t1", "title": "Llamar", "status": "pending", "case_id": null, "due_date": "2026-10-28T00:00:00", "is_overdue": false, "assigned_to_id": "u1"}),
        );
        assert!(t.case_id.is_none());
        assert_eq!(t.assigned_to_id.as_deref(), Some("u1"));
    }

    #[test]
    fn aviso_lee_ambas_banderas() {
        let n = notification_from(&json!({"id": "n1", "type": "task_assigned", "title": "Nueva tarea", "read": true}));
        assert!(n.is_read);
        assert_eq!(n.kind, "task_assigned");
    }

    #[test]
    fn comentario_borrado() {
        let c = comment_from(
            &json!({"id": "m1", "target_type": "case", "target_id": "c1", "content": null, "is_deleted": true, "reply_count": 2}),
        );
        assert!(c.content.is_none());
        assert_eq!(c.reply_count, 2);
        let m = mention_from(&json!({"id": "x", "target_type": "lead", "target_id": "l1", "is_read": false}));
        assert_eq!(m.target_type, "lead");
    }

    #[test]
    fn ticket_con_comentarios() {
        let t = ticket_from(&json!({
            "id": "t1", "ticket_number": "TK-0001", "title": "No imprime", "status": "open", "priority": "high",
            "tags": ["hw"], "sla_breached": false, "requester_email": "a@x.mx",
            "comments": [{"id": "c1", "content": "Ya lo vi", "is_internal": true, "source": "manual", "sender_name": "Mesa"}]
        }));
        assert_eq!(t.comments.len(), 1);
        assert_eq!(t.comments[0].author.as_deref(), Some("Mesa"));
        assert_eq!(t.tags, ["hw"]);
    }

    #[test]
    fn persona_del_crm_junta_correos_y_telefonos() {
        let p = crm_person_from(
            &json!({"id": "l1", "name": "Beto", "email": "b@x.mx", "email2": "", "email3": "b2@x.mx", "phone": "555", "status": "active"}),
        );
        assert_eq!(p.emails, ["b@x.mx", "b2@x.mx"]);
        assert_eq!(p.phones, ["555"]);
        assert_eq!(p.lead_type, "person");
    }

    #[test]
    fn correo_del_crm() {
        let e = crm_email_from(
            &json!({"id": "e1", "message_id": "<a@b>", "direction": "inbound", "to_emails": ["v@x.mx"], "matched_by": "token"}),
        );
        assert_eq!(e.message_id, "<a@b>");
        assert_eq!(e.to_emails, ["v@x.mx"]);
    }

    #[test]
    fn ids_numericos() {
        assert_eq!(id_of(&json!({"id": 7}), "id").as_deref(), Some("7"));
        assert_eq!(id_of(&json!({"id": ""}), "id"), None);
    }
}

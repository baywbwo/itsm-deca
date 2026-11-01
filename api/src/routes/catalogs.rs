//! Reference data: the global ticket status list and per-workspace catalogs.
//! Catalog reads go through the same workspace access check as every other scoped read.

use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use uuid::Uuid;

use crate::AppState;
use crate::domain::{TicketStatus, TicketStatusView};
use crate::error::AppError;
use crate::routes::workspaces::{ListBody, find_accessible, parse_id};
use crate::session::Session;

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct CatalogItem {
    id: Uuid,
    code: String,
    label: String,
    active: bool,
    sort_order: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogsBody {
    workspace_id: Uuid,
    priorities: Vec<CatalogItem>,
    categories: Vec<CatalogItem>,
    products: Vec<CatalogItem>,
    receiving_units: Vec<CatalogItem>,
    regions: Vec<CatalogItem>,
    intake_channels: Vec<CatalogItem>,
}

pub async fn statuses(_s: Session) -> Json<ListBody<TicketStatusView>> {
    Json(ListBody {
        items: TicketStatus::ALL.into_iter().map(Into::into).collect(),
    })
}

/// Table names are compile-time constants, never request input.
async fn catalog(state: &AppState, table: &str, ws: Uuid) -> Result<Vec<CatalogItem>, AppError> {
    let sql = format!(
        "SELECT id, code, label, active, sort_order FROM {table} \
         WHERE workspace_id = $1 ORDER BY sort_order, label, id"
    );
    Ok(sqlx::query_as::<_, CatalogItem>(&sql)
        .bind(ws)
        .fetch_all(&state.pool)
        .await?)
}

pub async fn list(
    State(state): State<AppState>,
    s: Session,
    Path(raw): Path<String>,
) -> Result<Json<CatalogsBody>, AppError> {
    let ws = find_accessible(&state, &s, parse_id(&raw)?).await?.id;
    Ok(Json(CatalogsBody {
        workspace_id: ws,
        priorities: catalog(&state, "priorities", ws).await?,
        categories: catalog(&state, "categories", ws).await?,
        products: catalog(&state, "products", ws).await?,
        receiving_units: catalog(&state, "receiving_units", ws).await?,
        regions: catalog(&state, "regions", ws).await?,
        intake_channels: catalog(&state, "intake_channels", ws).await?,
    }))
}

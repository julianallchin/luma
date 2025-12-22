use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use ts_rs::TS;

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternSummary {
    #[ts(type = "number")]
    pub id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    pub name: String,
    pub description: Option<String>,
    #[ts(type = "number | null")]
    #[sqlx(rename = "category_id")]
    pub category_id: Option<i64>,
    #[sqlx(rename = "category_name")]
    pub category_name: Option<String>,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

#[derive(TS, Serialize, Deserialize, Clone, Debug, FromRow)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternCategory {
    #[ts(type = "number")]
    pub id: i64,
    #[sqlx(rename = "remote_id")]
    pub remote_id: Option<String>,
    pub uid: Option<String>,
    pub name: String,
    #[sqlx(rename = "created_at")]
    pub created_at: String,
    #[sqlx(rename = "updated_at")]
    pub updated_at: String,
}

#[allow(dead_code)]
#[derive(TS, Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../src/bindings/schema.ts")]
#[ts(rename_all = "camelCase")]
pub struct PatternDetail {
    #[ts(type = "number")]
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub graph_json: String,
    pub created_at: String,
    pub updated_at: String,
}

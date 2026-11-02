//! Explicit initial local administrator, without synthetic records in operational mode.
use crate::{
    config::{Config, Environment},
    routes::password::hash_password,
};
use sqlx::PgPool;
use std::io::{self, Read};
use uuid::Uuid;

pub async fn run(pool: &PgPool, cfg: &Config) -> Result<(), String> {
    let username = std::env::var("ITSM_ADMIN_USERNAME")
        .map_err(|_| "ITSM_ADMIN_USERNAME wajib diisi")?
        .trim()
        .to_lowercase();
    if username.len() < 3 || username.len() > 254 {
        return Err("username tidak valid".into());
    }
    let confirm =
        std::env::var("ITSM_INIT_CONFIRM_DB").map_err(|_| "ITSM_INIT_CONFIRM_DB wajib diisi")?;
    let mut password = String::new();
    io::stdin()
        .take(1025)
        .read_to_string(&mut password)
        .map_err(|_| "password stdin gagal")?;
    let hash = hash_password(password.trim_end_matches(['\r', '\n']))?;
    drop(password);
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let current: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    if current != confirm {
        return Err("database confirmation mismatch".into());
    }
    let existing: i64 = sqlx::query_scalar("SELECT count(*) FROM local_credentials")
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    if existing > 0 {
        return Err("initial administrator already configured; refusing overwrite".into());
    }
    let expected = if cfg.environment == Environment::Production {
        "operational"
    } else {
        "test_dummy"
    };
    let marker: Option<String> = sqlx::query_scalar(
        "SELECT kind FROM itsm_deployment_marker WHERE marked_db=current_database()",
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    if marker.as_deref().is_some_and(|m| m != expected) {
        return Err("deployment marker mismatch".into());
    }
    if marker.is_none() {
        let tickets: i64 = sqlx::query_scalar("SELECT count(*) FROM tickets")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        if tickets != 0 || expected != "operational" {
            return Err("bootstrap requires empty operational database".into());
        }
        sqlx::query(
            "INSERT INTO itsm_deployment_marker(kind,marked_db) VALUES($1,current_database())",
        )
        .bind(expected)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    }
    let user: Uuid = sqlx::query_scalar(
        "INSERT INTO users(display_name,external_subject) VALUES('Administrator', $1) RETURNING id",
    )
    .bind(format!("local:{username}"))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO local_credentials(user_id,username,password_hash) VALUES($1,$2,$3)")
        .bind(user)
        .bind(username)
        .bind(hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    if expected == "operational" {
        for (code, name, brand) in [
            ("DOB", "DOB", "deca"),
            ("AJR", "PT Asia Jaya Remindo", "ajr"),
            ("BMT", "BMT", "bmt"),
        ] {
            let company: Uuid = sqlx::query_scalar(
                "INSERT INTO companies(code,name,brand_key) VALUES($1,$2,$3) RETURNING id",
            )
            .bind(code)
            .bind(name)
            .bind(brand)
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            sqlx::query("INSERT INTO workspaces(company_id,code,name) VALUES($1,$2,$3)")
                .bind(company)
                .bind(code)
                .bind(name)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let workspaces: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM workspaces WHERE active")
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    for ws in workspaces {
        let member:Uuid=sqlx::query_scalar("INSERT INTO workspace_memberships(workspace_id,user_id,role) VALUES($1,$2,'lead_cs') RETURNING id").bind(ws).bind(user).fetch_one(&mut *tx).await.map_err(|e|e.to_string())?;
        for grant in [
            "admin.members",
            "admin.settings",
            "integration.manage",
            "integration.credentials",
            "integration.recovery",
        ] {
            sqlx::query("INSERT INTO workspace_grants(workspace_id,membership_id,grant_code,granted_by) VALUES($1,$2,$3,$4)").bind(ws).bind(member).bind(grant).bind(user).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        }
        if expected == "operational" {
            for (table, entries) in [
                (
                    "priorities",
                    vec![
                        ("low", "Rendah"),
                        ("normal", "Normal"),
                        ("high", "Tinggi"),
                        ("urgent", "Mendesak"),
                    ],
                ),
                (
                    "intake_channels",
                    vec![
                        ("website", "Website"),
                        ("email", "Email"),
                        ("whatsapp", "WhatsApp"),
                    ],
                ),
                (
                    "categories",
                    vec![
                        ("qris", "QRIS"),
                        ("settlement", "Settlement"),
                        ("top_up", "Top up"),
                        ("account", "Akses akun"),
                        ("other", "Lainnya"),
                    ],
                ),
            ] {
                for (order, (code, label)) in entries.into_iter().enumerate() {
                    sqlx::query(&format!("INSERT INTO {table}(workspace_id,code,label,sort_order) VALUES($1,$2,$3,$4)")).bind(ws).bind(code).bind(label).bind(i32::try_from(order).map_err(|_|"order")?).execute(&mut *tx).await.map_err(|e|e.to_string())?;
                }
            }
        }
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    println!("Initial password administrator configured; no tickets generated");
    Ok(())
}

pub mod access;
pub mod ai_settings;
pub mod ai_suggestions;
pub mod attachments;
pub mod catalogs;
pub mod dashboard;
pub mod followup;
pub mod health;
pub mod intake;
pub mod integrations;
pub mod jira_webhook;
pub mod members;
pub mod mfa;
pub mod oidc;
pub mod password;
pub mod reports;
pub mod saved_views;
pub mod session;
pub mod settings;
pub mod sso_admin;
pub mod sync;
pub mod ticket_detail;
pub mod ticket_list;
pub mod tickets;
pub mod users;
pub mod workspaces;

use axum::Router;
use axum::routing::{delete, get, patch, post, put};

use crate::AppState;

/// All `/api/v1` routes, declared with full paths so the session layer sees them as-is.
/// Keep in sync with `openapi/openapi.json` (checked by `tests/contract.rs`).
pub const API_ROUTES: &[(&str, &str)] = &[
    ("post", jira_webhook::PATH),
    ("post", "/api/v1/workspaces/{workspace_id}/ai-suggestions"),
    ("get", "/api/v1/workspaces/{workspace_id}/ai-settings"),
    (
        "put",
        "/api/v1/workspaces/{workspace_id}/ai-settings/providers/{provider}",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/ai-settings/providers/{provider}/test",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/ai-settings/mcp-servers",
    ),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/ai-settings/mcp-servers/{server_id}",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/ai-settings/mcp-servers/{server_id}/test",
    ),
    ("post", "/api/v1/login"),
    ("post", "/api/v1/login/mfa"),
    ("get", "/api/v1/auth/oidc/{company_code}/start"),
    ("get", "/api/v1/auth/oidc/callback"),
    ("get", "/api/v1/meta"),
    ("post", "/api/v1/uat/session"),
    ("get", "/api/v1/session"),
    ("delete", "/api/v1/session"),
    ("post", "/api/v1/session/csrf"),
    ("post", "/api/v1/session/logout-all"),
    ("post", "/api/v1/session/step-up"),
    ("post", "/api/v1/account/password"),
    ("get", "/api/v1/me/mfa"),
    ("delete", "/api/v1/me/mfa"),
    ("post", "/api/v1/me/mfa/enrollment"),
    ("post", "/api/v1/me/mfa/enrollment/{enrollment_id}/confirm"),
    ("post", "/api/v1/me/mfa/recovery-codes"),
    ("get", "/api/v1/users"),
    ("post", "/api/v1/users"),
    ("patch", "/api/v1/users/{user_id}"),
    ("post", "/api/v1/users/{user_id}/password-reset"),
    ("post", "/api/v1/users/{user_id}/mfa-reset"),
    ("post", "/api/v1/users/{user_id}/unlock"),
    ("put", "/api/v1/users/{user_id}/company"),
    ("get", "/api/v1/platform/companies"),
    ("get", "/api/v1/platform/sso"),
    ("put", "/api/v1/platform/companies/{company_id}/sso"),
    ("put", "/api/v1/platform/companies/{company_id}/sso/secret"),
    ("post", "/api/v1/platform/companies/{company_id}/sso/test"),
    ("put", "/api/v1/platform/companies/{company_id}/sso/enabled"),
    (
        "get",
        "/api/v1/platform/companies/{company_id}/sso/identities",
    ),
    (
        "post",
        "/api/v1/platform/companies/{company_id}/sso/identities",
    ),
    (
        "delete",
        "/api/v1/platform/companies/{company_id}/sso/identities/{identity_id}",
    ),
    ("get", "/api/v1/ticket-statuses"),
    ("get", "/api/v1/permissions"),
    ("get", "/api/v1/workspaces"),
    ("get", "/api/v1/workspaces/{workspace_id}"),
    ("get", "/api/v1/workspaces/{workspace_id}/members"),
    ("get", "/api/v1/workspaces/{workspace_id}/catalogs"),
    ("get", "/api/v1/workspaces/{workspace_id}/permissions"),
    ("get", "/api/v1/workspaces/{workspace_id}/grants"),
    ("get", "/api/v1/workspaces/{workspace_id}/dashboard"),
    ("get", "/api/v1/workspaces/{workspace_id}/saved-views"),
    ("post", "/api/v1/workspaces/{workspace_id}/saved-views"),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/saved-views/{saved_view_id}",
    ),
    (
        "delete",
        "/api/v1/workspaces/{workspace_id}/saved-views/{saved_view_id}",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/intake-directory"),
    (
        "put",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/simulation",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/retry",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/reconcile",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/tickets"),
    ("post", "/api/v1/workspaces/{workspace_id}/tickets"),
    ("get", "/api/v1/workspaces/{workspace_id}/tickets/board"),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}",
    ),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/detail",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/activity",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/related-work",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/related-work/{related_work_id}/completion",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/transitions",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment-history",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/integrations",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments/{attachment_id}",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/reports/preview"),
    ("get", "/api/v1/workspaces/{workspace_id}/report-runs"),
    ("post", "/api/v1/workspaces/{workspace_id}/report-runs"),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/report-runs/{report_run_id}",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/report-runs/{report_run_id}/content",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/notes",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/notes",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/conversations",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments/{attachment_id}/content",
    ),
    ("patch", "/api/v1/workspaces/{workspace_id}/settings"),
    ("get", "/api/v1/workspaces/{workspace_id}/settings"),
    ("get", "/api/v1/companies"),
    ("post", "/api/v1/companies"),
    ("patch", "/api/v1/companies/{company_id}"),
    ("patch", "/api/v1/companies/{company_id}/profile"),
    ("get", "/api/v1/workspaces/{workspace_id}/catalogs/{kind}"),
    ("post", "/api/v1/workspaces/{workspace_id}/catalogs/{kind}"),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/catalogs/{kind}/{item_id}",
    ),
    ("post", "/api/v1/workspaces/{workspace_id}/members"),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/members/{membership_id}",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/members/{membership_id}/grants",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/members/{membership_id}/grants/revoke",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/integrations"),
    ("post", "/api/v1/workspaces/{workspace_id}/integrations"),
    (
        "patch",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/test",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/activate",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/disable",
    ),
    (
        "put",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/config",
    ),
    (
        "put",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/credential",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/webhook-secret",
    ),
    (
        "delete",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/webhook-secret",
    ),
    (
        "delete",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/credential",
    ),
    (
        "get",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/options",
    ),
    (
        "post",
        "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/directory/sync",
    ),
    ("get", "/api/v1/workspaces/{workspace_id}/intake/jira-users"),
];

pub fn api_router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/login", post(password::login))
        .route("/api/v1/login/mfa", post(mfa::login_mfa))
        .route(
            "/api/v1/auth/oidc/{company_code}/start",
            get(oidc::start),
        )
        .route("/api/v1/auth/oidc/callback", get(oidc::callback))
        .route("/api/v1/workspaces/{workspace_id}/ai-suggestions", post(ai_suggestions::suggest))
        .route(
            "/api/v1/workspaces/{workspace_id}/ai-settings",
            get(ai_settings::list),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/ai-settings/providers/{provider}",
            put(ai_settings::save_provider),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/ai-settings/providers/{provider}/test",
            post(ai_settings::test_provider),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/ai-settings/mcp-servers",
            post(ai_settings::create_mcp),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/ai-settings/mcp-servers/{server_id}",
            patch(ai_settings::update_mcp),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/ai-settings/mcp-servers/{server_id}/test",
            post(ai_settings::test_mcp),
        )
        .route("/api/v1/meta", get(session::meta))
        .route("/api/v1/uat/session", post(session::uat_enter))
        .route(
            "/api/v1/session",
            get(session::current).merge(delete(session::logout)),
        )
        .route("/api/v1/session/csrf", post(session::refresh_csrf))
        .route("/api/v1/session/logout-all", post(session::logout_all))
        .route("/api/v1/session/step-up", post(mfa::step_up))
        .route("/api/v1/account/password", post(password::change_password))
        .route(
            "/api/v1/me/mfa",
            get(mfa::get_status).merge(delete(mfa::delete_mfa)),
        )
        .route("/api/v1/me/mfa/enrollment", post(mfa::start_enrollment))
        .route(
            "/api/v1/me/mfa/enrollment/{enrollment_id}/confirm",
            post(mfa::confirm_enrollment),
        )
        .route(
            "/api/v1/me/mfa/recovery-codes",
            post(mfa::regenerate_recovery_codes),
        )
        .route("/api/v1/users", get(users::list).merge(post(users::create)))
        .route("/api/v1/users/{user_id}", patch(users::update))
        .route("/api/v1/users/{user_id}/password-reset", post(users::reset_password))
        .route("/api/v1/users/{user_id}/mfa-reset", post(users::mfa_reset))
        .route("/api/v1/users/{user_id}/unlock", post(users::unlock))
        .route("/api/v1/users/{user_id}/company", put(users::set_company))
        .route("/api/v1/platform/companies", get(users::companies))
        .route("/api/v1/platform/sso", get(sso_admin::overview))
        .route("/api/v1/platform/companies/{company_id}/sso", put(sso_admin::save))
        .route(
            "/api/v1/platform/companies/{company_id}/sso/secret",
            put(sso_admin::put_secret),
        )
        .route(
            "/api/v1/platform/companies/{company_id}/sso/test",
            post(sso_admin::test),
        )
        .route(
            "/api/v1/platform/companies/{company_id}/sso/enabled",
            put(sso_admin::set_enabled),
        )
        .route(
            "/api/v1/platform/companies/{company_id}/sso/identities",
            get(sso_admin::identities).merge(post(sso_admin::link)),
        )
        .route(
            "/api/v1/platform/companies/{company_id}/sso/identities/{identity_id}",
            delete(sso_admin::unlink),
        )
        .route("/api/v1/ticket-statuses", get(catalogs::statuses))
        .route("/api/v1/permissions", get(access::catalog))
        .route("/api/v1/workspaces", get(workspaces::list))
        .route(
            "/api/v1/workspaces/{workspace_id}",
            get(workspaces::get_one),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/members",
            get(members::list).merge(post(members::add)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/catalogs",
            get(catalogs::list),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/permissions",
            get(access::mine),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/grants",
            get(access::list_grants),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/dashboard",
            get(dashboard::get),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/saved-views",
            get(saved_views::list).merge(post(saved_views::create)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/saved-views/{saved_view_id}",
            patch(saved_views::rename).merge(delete(saved_views::delete)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets",
            get(ticket_list::list).merge(post(intake::create)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/board",
            get(ticket_list::board),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}",
            get(tickets::get_one).merge(patch(ticket_detail::update_properties)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/detail",
            get(ticket_detail::detail),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/activity",
            get(ticket_detail::activity),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/related-work",
            post(ticket_detail::create_related_work),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/related-work/{related_work_id}/completion",
            post(ticket_detail::complete_related_work),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/transitions",
            post(ticket_list::transition),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment",
            post(ticket_list::assign),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/assignment-history",
            get(intake::assignment_history),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/integrations",
            get(sync::ticket_sync),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/intake-directory",
            get(intake::directory),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/simulation",
            put(sync::set_simulation),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/retry",
            post(sync::retry),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integration-outbox/{outbox_id}/reconcile",
            post(sync::reconcile),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments/{attachment_id}",
            get(tickets::attachment),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/report-runs/{report_run_id}",
            get(reports::get_run),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/reports/preview",
            get(reports::preview),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/report-runs",
            get(reports::list_runs).post(reports::create_run),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/report-runs/{report_run_id}/content",
            get(reports::download),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/notes",
            get(followup::list_notes).post(followup::create_note),
        )
        .route("/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/conversations", get(followup::conversations))
        .route(
            "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments/{attachment_id}/content",
            get(attachments::download),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/settings",
            get(settings::get_settings).merge(patch(settings::update_settings)),
        )
        .route(
            "/api/v1/companies",
            get(settings::list_companies).merge(post(settings::create_company)),
        )
        .route(
            "/api/v1/companies/{company_id}",
            patch(settings::set_company_active),
        )
        .route(
            "/api/v1/companies/{company_id}/profile",
            patch(settings::update_company),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/catalogs/{kind}",
            get(settings::list_catalog).merge(post(settings::create_catalog_entry)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/catalogs/{kind}/{item_id}",
            patch(settings::update_catalog_entry),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/members/{membership_id}",
            patch(members::update),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/members/{membership_id}/grants",
            post(members::grant),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/members/{membership_id}/grants/revoke",
            post(members::revoke),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations",
            get(integrations::list).merge(post(integrations::create)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}",
            patch(integrations::update),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/test",
            post(integrations::test),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/activate",
            post(integrations::activate),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/disable",
            post(integrations::disable),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/config",
            put(integrations::configure),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/credential",
            put(integrations::put_credential).merge(delete(integrations::delete_credential)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/webhook-secret",
            post(integrations::rotate_webhook_secret)
                .merge(delete(integrations::delete_webhook_secret)),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/options",
            get(integrations::options),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/integrations/{connection_id}/directory/sync",
            post(integrations::sync_directory),
        )
        .route(
            "/api/v1/workspaces/{workspace_id}/intake/jira-users",
            get(intake::jira_users),
        )
}

/// The upload route streams large bodies, so it is mounted outside the global 64 KiB body
/// limit and 10 s timeout (it enforces its own size cap and gets a longer timeout).
pub fn upload_router() -> Router<AppState> {
    Router::new().route(
        "/api/v1/workspaces/{workspace_id}/tickets/{ticket_id}/attachments",
        post(attachments::upload),
    )
}

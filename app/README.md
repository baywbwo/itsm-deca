# DOB ITSM Platform — milestone F1

Run from repository root using Node 24.22.1. `npm test` executes real HTTP and SQLite tests; `npm start` serves on http://127.0.0.1:3100. No npm install needed. `/healthz`, `/version`; env APP_DB, HOST, PORT, APP_COMMIT, COOKIE_SECURE. Full provisioning/backup/rollback contract: ../docs/DEPLOY.md.

Fixtures are explicit opt-in using APP_FIXTURES=1 and a temporary FIXTURE_PASSWORD environment value (minimum 14 chars), then `npm run fixture`. Set the same APP_DB for provisioning and server. Never use a production DB or accounts. Fixture roles: opr, sup, l2, l3, cmr, aud, adm, each @example.test. Password is supplied locally and never printed. Fixture accounts can log in via UI; raw database access is not needed for ordinary review.

API contract: public POST /api/intake requires Idempotency-Key; GET /api/intake/:token returns only processing status/number. POST /api/login, POST /api/logout, GET /api/me. Internal routes: /api/tickets, /api/tickets/:id with PATCH action triage/assign/status/escalate and optimistic version; /messages, /history, /attachments, /promote. /api/reports/:id/approve advances one state with checker note/version; POST /api/export requires from, until (exclusive WIB), reason, purpose. /api/knowledge, /api/dashboard, /api/outbox, /api/audit. Config and role edits have no endpoint in this milestone; closed by default.

Test clock: import createApp({dbPath, now:()=>clock,fixture:true}) from app/server.js; server.listen(0,'127.0.0.1'). Domain jobs are explicit via app.domain.jobs(failMock=false), so independent QA can advance fake time without any test endpoint enabled in deployed server. CLI worker runs every second. No external network adapters exist in this milestone; outbox mock delivers to local mock_deliveries with unique event key. Backoff 2^attempt seconds, budget five, dead state visible to permitted users.

Known coverage limits are listed in DEPLOY.md. Tests do not substitute for independent QA or actual UI/UX browser review. No claim of regulatory conformity, live integration, completed full F1, migration or release readiness.

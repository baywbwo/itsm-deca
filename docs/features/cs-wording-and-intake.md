# CS wording and complaint intake

The existing application shell and per-company brand identity are retained. Shared action buttons and focus states use a stronger blue. The catalog administration tab is removed; catalog data, APIs, existing tickets and reporting references remain intact.

## Website and internal intake fields

Both intake experiences use merchant/business name, registered email, PIC/reporter name, optional WhatsApp, category and chronology. The internal form also supports an explicit subject and received channel. Public website submissions must be mapped to the server-configured company/workspace and Website channel; visitors must never choose arbitrary workspace IDs. If the website lacks a subject, its adapter should derive a bounded subject from the category and first chronology line.

Manual contact fields are stored as a labelled prefix in the existing private ticket chronology, matching the legacy application. They do not create a CIF identity or update the merchant directory. The separate merchant reference remains an explicit workspace-scoped directory link. Contact fields are optional internally, to allow CS to record complaints received without a complete customer identity. The combined contact prefix and chronology respect the server's 20,000-character limit. Existing idempotency keys and in-memory draft isolation continue to apply.

This change aligns intake fields; it does not redirect the legacy port-3000 company-profile form to the Rust API. That existing public endpoint must remain intact until its authenticated server-to-server adapter and anti-abuse controls are implemented and tested. Do not expose a UAT-superadmin session, Basic Auth credential, OpenAI key, or internal ticket endpoint in public JavaScript.

## GPT drafting assistance

POST `/api/v1/workspaces/{workspace_id}/ai-suggestions`, protected by the existing session, CSRF and `ticket.create` permission. Input contains only an explicit `mode` (`chronology` or `reply`) and the user-selected text (up to 6,000 characters). The service does not fetch ticket histories, contact records, other workspaces or attachments. Common email and telephone/identifier-shaped number patterns are masked before transmission; masking is best-effort and does not guarantee PII removal. The UI asks CS to remove sensitive content before submitting.

The GPT Responses API request uses `store: false`, no tools, a fixed official HTTPS endpoint, no redirects, 12-second timeout, 1,200 output tokens, a 64-KiB response bound, and two concurrent requests per process. Existing per-user write rate limits also apply. Prompts require fact preservation and prohibit invented promises, deadlines or verification claims. Returned drafts require explicit review and application. Changed source text prevents applying a stale suggestion. Applying a draft never saves the ticket or sends a customer message; the note composer remains an internal-note composer.

Set `OPENAI_API_KEY` and `ITSM_OPENAI_MODEL` in the API service environment, then restart the service. Select an available GPT model explicitly (`gpt-` model ID); there is no implicit model substitution. Keep secrets on the server. If either setting is absent, or the provider fails/refuses/returns incomplete output, the endpoint returns 503 and the original draft remains intact. No automatic retry that could duplicate provider cost is performed.

Live generation has not been verified: the deployed ITSM runtime had neither variable configured at inspection time. A ChatGPT/Codex browser subscription is not used as an application API credential.

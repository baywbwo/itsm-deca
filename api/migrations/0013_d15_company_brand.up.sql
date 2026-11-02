-- D15 (expand only): per-company UI branding. `brand_key` selects one of the logo + palette
-- presets bundled with the web app (controlled assets, never a URL or an uploaded file);
-- NULL = not configured, the UI shows a neutral look. `brand_name` is an optional display name
-- (NULL = the company name). Existing companies stay unconfigured until an administrator
-- picks a preset; nothing is guessed from company codes or names.
ALTER TABLE companies
    ADD COLUMN brand_key text CHECK (brand_key IN ('deca', 'ajr')),
    ADD COLUMN brand_name text CHECK (brand_name IS NULL OR length(btrim(brand_name)) BETWEEN 1 AND 60);

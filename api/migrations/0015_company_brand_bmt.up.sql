ALTER TABLE companies DROP CONSTRAINT companies_brand_key_check;
ALTER TABLE companies ADD CONSTRAINT companies_brand_key_check CHECK (brand_key IN ('deca', 'ajr', 'bmt'));

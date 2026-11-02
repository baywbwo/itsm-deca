DO $$ BEGIN
  IF EXISTS (SELECT 1 FROM companies WHERE brand_key = 'bmt') THEN
    RAISE EXCEPTION 'Select another branding preset for BMT companies before downgrading';
  END IF;
END $$;
ALTER TABLE companies DROP CONSTRAINT companies_brand_key_check;
ALTER TABLE companies ADD CONSTRAINT companies_brand_key_check CHECK (brand_key IN ('deca', 'ajr'));

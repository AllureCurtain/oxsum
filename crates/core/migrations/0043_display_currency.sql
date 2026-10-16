-- Multi-currency display (issue #257): an organization's presentational
-- fiat conversion — the ledger still only ever holds integer minor units of
-- one credit, and a bill's proof covers what the entry charged, never what it
-- was rendered as. These two columns name what a credit *displays* as: the
-- fiat unit (`display_currency`, an ISO-4217-shaped code) and the rate at
-- which one credit converts (`display_rate_per_credit`, a decimal rendered
-- into text — no float ever touches money, and the stored text is what the
-- API answers back).
--
-- Both NULL means the organization renders credits only — the default every
-- existing row keeps. They move together by the setter's rule: a rate
-- without a unit to read it in is meaningless, so the write API takes them
-- as one field rather than two columns a caller could set apart.

ALTER TABLE oxsum.organizations
    ADD COLUMN display_currency text;

-- ISO 4217 is three letters; a longer code names nothing the page could show.
ALTER TABLE oxsum.organizations
    ADD CONSTRAINT organizations_display_currency_code
    CHECK (display_currency IS NULL OR display_currency ~ '^[A-Z]{3}$');

ALTER TABLE oxsum.organizations
    ADD COLUMN display_rate_per_credit text;

-- A decimal literal: digits with at most one point, and no exponent, sign or
-- thousand separator — the form `DisplayRate` parses, so what stores is what
-- converts. Non-positive rates are refused by the setter, not the check:
-- the constraint guards the shape, the code guards the meaning.
ALTER TABLE oxsum.organizations
    ADD CONSTRAINT organizations_display_rate_per_credit_decimal
    CHECK (display_rate_per_credit IS NULL OR display_rate_per_credit ~ '^[0-9]+(\.[0-9]+)?$');

-- The pair moves together: a currency with no rate and a rate with no
-- currency are both a half-configured display, which no read may see.
ALTER TABLE oxsum.organizations
    ADD CONSTRAINT organizations_display_pair
    CHECK ((display_currency IS NULL) = (display_rate_per_credit IS NULL));

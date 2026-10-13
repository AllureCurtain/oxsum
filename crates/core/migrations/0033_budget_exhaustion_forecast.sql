-- Forecasted budget exhaustion (issue #220): `budget.exhaustion_forecast`
-- joins the webhook event catalog.
--
-- The event fires once per key per budget window, so the dedup state hangs on
-- the key — not the endpoint the way `balance.low`'s latch does (one endpoint
-- threshold, one latch). A key's forecast is judged against the window its
-- `budget_duration` opens: the row records which window start the forecast was
-- sent for, so the same key forecasts again in the next period, and a changed
-- constraint (`spend_limit_minor`/`budget_duration` re-PATCHed) clears the
-- latch so the new terms forecast from scratch.
ALTER TABLE oxsum.api_keys
    ADD COLUMN budget_forecast_sent_for date;

# Business merchant setup UI (#831)

N06's business Integrations surface now provides complete create/version/status
forms, paginated redacted history and event reads, retained callback coordinates
and explicit test/live mode. Identity stays immutable. Owner-only credential
operations carry a reason and stable unchanged retry key. Changing a rail clears
its draft; closing or losing owner permission discards credentials. ReportsRead
roles can reach merchant reads without mounting integration-key controls.

The server APIs, immutable versions and signing authority are unchanged. No
provider request is made by saving. Order creation still uses the environment
bootstrap selection; a newly enabled UI-created merchant is not automatically
the checkout default. That selection is a separate N06 implementation unit.

Validation: two draft tests cover redacted secret initialization, complete
rail-specific fields and exact secret text; four OpenAPI security checks passed;
code generation retains the existing contract. TypeScript/Vite build passed.
Independent review identified the inherited owner-only navigation gate; it was
fixed to ReportsRead with integration-key controls still owner-only. The final
incremental source review passed. Runtime HTTP fixture results are recorded in
the PR validation after the shared Cargo window becomes available.

Actual browser desktop/mobile, keyboard, both locales and interrupted submission
checks remain unperformed: the authorized browser task space is unavailable.
These source/build checks are not J05 or genuine PSP certification.

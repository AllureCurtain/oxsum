# Business section reload repair

Issue [#955](https://github.com/AllureCurtain/oxsum/issues/955), found during #946.
The router strips its `/app` basename from `location.pathname`. Comparing that
value with `/app/business` redirected an already selected Business page to
`/business`, dropping its section hash on reload. Compare with the router-relative
`/business` instead. Role and context checks are unchanged.

The production frontend build passed. In the isolated #946 browser runtime,
`/app/business#business/customers` remained on Customers through desktop and
390×844 mobile reloads, with viewport/document width both 390 pixels. A Business
session opening `/app/bills` still redirected to `/app/business`. The screenshot
is retained with #946. This frontend was built from `615249d0` plus this one-line
fix and served by the staff-preview repair binary `013bc8cb`; it is explicitly
development frontend evidence, not an exact integrated release candidate.

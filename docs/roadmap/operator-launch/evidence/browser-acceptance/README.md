# Isolated development browser acceptance

Issue [#946](https://github.com/AllureCurtain/oxsum/issues/946). Observed
2026-11-10 with Ego Lite. Actual portal actions, including failed paths and
their repairs, are recorded here. Final candidate and external gates remain.

## Runtime identity

Initial source `98238b38ef3708000233aebc606ad286f701be0f`, schema
`0122_statement_repayment`. A dedicated PostgreSQL database, owner token,
credentials and frozen binary were created. After minting the owner token,
configuration and onboarding used visible browser forms. Model supply was an
owned loopback fixture; mail went only to local Mailpit and `.invalid` addresses.
No real-provider or PSP request was made. The original shared provider budget
remains **2/5 attempts**, from #944, unchanged.

Staff retry used source `013bc8cb`, binary SHA-256
`d90c5e178ded6b8e3fe5d9045736f739329a7b57c733907764f020d444f09b37`,
with the same database and original invitations. Section-link retry used
frontend `615249d0` plus #955 on that backend. These identified development
versions are not one exact release artifact.

## Observed journeys

| Path | Observed outcome |
| --- | --- |
| Fresh owner | Wrong email refused with draft retained; corrected email initialized the business, retained invite-only registration and allowed normal sign-in/Business setup. |
| Profile | Name, support address, help URL and `#234567` saved as revision 1; reload retained them and branding updated. Desktop and 390×844 mobile inspected. |
| Channel/model | Owner approved the owned loopback IP and created `browser-chat`; rates `1.000001` / `2.000003` credits per million, max output 32. Detail returned only credential last four. |
| Customer sale | Created draft 1, published immutable version 1, explicitly activated an immediate open-ended default. Rates `3.000007` / `4.000009` stayed exact. Preview of 9 input/3 output tokens returned 40 credit-minor units and the selected book/version/schedule. |
| Collection | Saved `1 credit = 0.250001 USD`, revision 1; reload retained it. No selected merchant, so checkout remained incomplete. |
| Customer invitation | Owner submitted one local email. Actual link refused wrong email, then registered the reserved customer. One key shown once, zero balance, no staff seat. Actual verification mail verified the address; history became accepted. |
| Registration policy | Owner deliberately enabled open registration with a reason; reload retained it. Restored invite-only and verified saved policy. |
| Business readiness | Owner 4/5 with missing checkout named. Finance 3/5 and Support 2/5 accurately marked inaccessible configuration. |
| Staff onboarding | Initial Finance preview returned HTTP 500 and disabled consent. After #950, original Finance and Support links registered staff-only identities; normal sign-in reached the proper role. Wrong signed-in email refused. No customer key/funds returned. Finance had no secret editor; Support lacked payments/reports/staff navigation. |
| Revocation | Owner revoked Support by name/reason. Subsequent login had no usable Business console; accepted invitation returned invalid preview with acceptance disabled. No role restored. |
| Mobile/keyboard | Setup/profile/customers had document width 390 at 390px viewport; readable forms and horizontal navigation. Keyboard Enter activated profile navigation with visible focus. |
| Section reload | Baseline lost its Business section. #956 retained Customers through desktop/mobile reloads. Business sessions opening `/app/bills` still redirected to Business. |

## Defects and remaining acceptance

- Staff preview and incorrect customer-consent wording: #950. Baseline failure
  reproduced; 28 focused and 2,191 full tests passed, strict core/server Clippy
  and actual browser retry passed.
- Business hash loss: #956. Frontend build and actual browser checks passed.
- Initial customer getting-started still asked for the operator token; #916 is
  the existing repair. Its unmerged readiness flow is not passed by this run.
- Merchant creation was inspected but not saved. HTTPS SDK/backend, automatic
  callbacks, selected checkout and real PSP sandbox remain. Local SMTP is not
  external inbox certification.
- First-call settlement/evidence, commercial plan/bundle/refund/statement and
  embedded consent/notification browser journeys remain on their integrated
  code. Existing executable evidence does not replace these browser gates.
- No final candidate, reference eight-hour soak, outside pilot, publication or
  existing-service upgrade follows from these development observations.

## Images

Each was visually inspected and contains disposable identities, no secrets:
`profile-desktop.png`, `profile-mobile.png`, `setup-mobile.png`,
`staff-preview-failure.png`, `finance-role.png`, `customers-mobile-reload.png`.

import assert from "node:assert/strict";
import test from "node:test";
import { applicationLinkToken } from "../src/lib/application-link.ts";
import { credentialRequest, type GrantDraft } from "../src/lib/integration-grant.ts";
const secret = `oxs-lnk-${"a".repeat(64)}`;
const now = 1_000_000;
test("only an exact fragment secret or unexpired tab record can resume consent", () => {
  const retained = JSON.stringify({ token: secret, until: now + 100 });
  assert.equal(applicationLinkToken(`#${secret}`, null, now), secret);
  assert.equal(applicationLinkToken("", retained, now), secret);
  for (const fragment of [secret, `#token=${secret}`, "#invalid", `#${secret}&next=other`]) assert.equal(applicationLinkToken(fragment, retained, now), "");
  for (const value of ["null", "{", JSON.stringify({ token: secret, until: now }), JSON.stringify({ token: secret, until: now + 8 * 86_400_000 }), JSON.stringify({ token: "bad", until: now + 100 })]) assert.equal(applicationLinkToken("", value, now), "");
});
test("verified ownership invitation scope is confined to explicit application grants", () => {
  const draft: GrantDraft = { mode: "application", applicationId: "app", customerIds: [], scopes: ["customerLinkWrite"], name: "link", days: "7", reason: "offer ownership" };
  assert.deepEqual(credentialRequest(draft, "key", now).scopes, ["customerLinkWrite"]);
  assert.throws(() => credentialRequest({ ...draft, mode: "customers", customerIds: ["org"] }, "key", now), /scopes/);
});

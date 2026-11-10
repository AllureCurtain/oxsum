import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import * as yaml from "js-yaml";

const api = yaml.load(readFileSync(new URL("../../crates/server/openapi.yaml", import.meta.url), "utf8"));
const methods = new Set(["get", "put", "post", "delete", "options", "head", "patch", "trace"]);
const consumptionSchemes = ["bearer", "apiKeyHeader", "googApiKeyHeader"];
const operations = Object.entries(api.paths).flatMap(([path, item]) =>
  Object.entries(item)
    .filter(([method]) => methods.has(method))
    .map(([method, operation]) => ({ name: `${method.toUpperCase()} ${path}`, operation })),
);
const requirements = (operation) => operation.security ?? api.security ?? [];

test("every security requirement names a declared scheme", () => {
  const declared = new Set(Object.keys(api.components.securitySchemes));
  const unknown = [{ name: "API defaults", operation: api }, ...operations].flatMap(({ name, operation }) =>
    requirements(operation).flatMap((requirement) =>
      Object.keys(requirement).filter((scheme) => !declared.has(scheme)).map((scheme) => `${name}: ${scheme}`),
    ),
  );
  assert.deepEqual(unknown, []);
});

test("a cookie is never required alongside an explicit machine credential", () => {
  const mixed = operations.filter(({ operation }) => requirements(operation).some((requirement) =>
    "sessionCookie" in requirement && Object.keys(requirement).some((scheme) => scheme !== "sessionCookie"),
  )).map(({ name }) => name);
  assert.deepEqual(mixed, []);
});

test("operations denying consumption access do not advertise a consumption credential", () => {
  const contradictory = operations.filter(({ operation }) =>
    operation["x-consumption-key-access"] === "denied" && requirements(operation).some((requirement) =>
      consumptionSchemes.some((scheme) => scheme in requirement),
    ),
  ).map(({ name }) => name);
  assert.deepEqual(contradictory, []);
});

// These expectations follow the server's existing route and customer-capability
// audit. Published commercial metadata stays readable with a consumption key;
// private account records and payment actions require a current personal session.
test("commercial and financial operations retain their audited credential boundaries", () => {
  const scopedReads = ["/plans", "/plans/{planVersionId}", "/bundles", "/org/promo-lots"];
  const personal = [
    ["get", "/org/readiness/receipt"],
    ["get", "/org/readiness/serving"],
    ["get", "/org/plan"],
    ["post", "/org/plan/cancel"],
    ["post", "/org/plan/change"],
    ["get", "/org/statements"],
    ["get", "/org/statements/{statementId}"],
    ["get", "/org/plan/journals"],
    ["get", "/org/plan/journals/{journalId}"],
    ["post", "/orders"],
    ["get", "/orders"],
    ["get", "/orders/{orderId}"],
    ["get", "/orders/{orderId}/fulfillment"],
    ["get", "/orders/{orderId}/checkout-session"],
    ["post", "/orders/{orderId}/refund-request"],
    ["get", "/orders/{orderId}/refunds"],
    // AccountManage additionally checks owner/admin membership in the runtime.
    ["get", "/org/usage-report"],
  ];
  const readAlternatives = [{ sessionCookie: [] }, ...consumptionSchemes.map((scheme) => ({ [scheme]: [] }))];
  for (const path of scopedReads) {
    const operation = api.paths[`/api/v1${path}`].get;
    assert.deepEqual(operation.security, readAlternatives, `GET ${path}`);
    assert.equal(operation["x-consumption-key-access"], "own-records-or-metadata", `GET ${path}`);
  }
  for (const [method, path] of personal) {
    const operation = api.paths[`/api/v1${path}`][method];
    assert.deepEqual(operation.security, [{ sessionCookie: [] }], `${method} ${path}`);
    assert.equal(operation["x-consumption-key-access"], "denied", `${method} ${path}`);
    assert.ok(operation.responses["403"], `${method} ${path} declares the authority refusal`);
  }
});


test("application statement history requires explicit scoped billing authority", () => {
  for (const suffix of ["", "/{statementId}"]) {
    const operation = api.paths["/api/v1/integration/customers/{organizationId}/statements" + suffix].get;
    assert.deepEqual(operation.security, [{ integrationCredential: [] }]);
    assert.equal(operation["x-integration-scope"], "billingRead");
    assert.ok(operation.responses["404"]);
    assert.ok(operation.responses["409"]);
  }
});

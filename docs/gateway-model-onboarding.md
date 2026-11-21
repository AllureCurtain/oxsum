# Bring a model online through the Oxsum gateway

Oxsum is a self-hosted LLM gateway with usage-based billing. It routes supported requests to configured providers, reserves funds or entitlement before serving, and settles recorded usage against the terms selected at admission. Customers can check the resulting receipt within the documented [proof scope](proofs.md).

| What you see | What it tells you |
| --- | --- |
| **Upstream model ID** | The provider listed this ID for the credential used to query its catalog, or supplied the ID in its own documentation. Use it as a candidate for configuration. |
| **Configured model route** in **Business → Channels** | The channel has a saved model version with a billing mode, rates and routing settings. The channel protocol and billing mode determine the gateway endpoint. |
| **Customer-visible model** in Oxsum's `GET /v1/models` | A priced route or alias is eligible for this customer and key under the configured tier, model allowlists, channel group and applicable sale coverage. The result comes from Oxsum and requires no upstream request. |

Only a real call exercises request admission, provider inference and settlement. For example, a model can appear in the customer catalog and a particular call can still exceed the key's budget or hit an upstream rate limit.

## First call

1. **Save a provider connection.** In **Business → Channels**, the business
   owner sets the protocol, base URL, destination policy and serving credential.
   Saving is local. **Check reachability** sends one authenticated `HEAD`
   request to the saved endpoint. For example, `405 Method Not Allowed` still
   confirms the endpoint responded. It tells you nothing about whether a model
   accepts an inference request.
   [Business provider connections](user-guide.md#business-provider-connections)
2. **Find the model ID.** Use the provider's documentation or account console.
   If your build offers **Discover upstream models**, the business owner can
   list candidates from a saved OpenAI-compatible, Anthropic or Gemini channel
   and pick one to prefill the model form. Enter a known ID manually if the
   model is absent or the provider does not support listing.
   [Discovery issue #1249](https://github.com/AllureCurtain/oxsum/issues/1249)
3. **Configure a model route.** Under **Configured model routes**, select **Add
   model**. Enter the upstream ID, billing mode and rates for the intended
   operation, and set the output limit where applicable. The saved version can
   serve eligible traffic immediately, so check access and any applicable
   customer sale terms before saving.
   [Business models, aliases and routing](user-guide.md#business-models-aliases-and-routing) ·
   [Prepare customer sale books](user-guide.md#prepare-customer-sale-books)
4. **Check the customer catalog and send a sample.** Use the intended customer's
   consumption key with `GET /v1/models` and confirm the public ID, including an
   alias if configured. If the ID is missing, check the route, customer tier,
   key allowlist and channel group, and any sale schedule in use. Send a small
   request to the endpoint that matches the channel protocol and billing mode.
   **Get started** can preview funding for a sample chat. Open the successful
   call's receipt from **Requests** or **Bills**.
   [Gateway endpoints](integrations.md#llm-gateway) ·
   [Runnable customer example](../examples/consumption/README.md) ·
   [Check sample funding](user-guide.md#check-your-model-and-sample-funding) ·
   [Verify your first successful call](user-guide.md#verify-your-first-successful-call)

## Design references

| Gateway | Practice relevant to Oxsum |
| --- | --- |
| New API | Saved-channel discovery and channel testing are distinct operator actions. Keep both discoverable from the channel, with labels that say what the action does. [Upstream model list](https://docs.newapi.ai/en/docs/api/management/channel-management/channel-fetch_models-id-get) · [Channel management](https://docs.newapi.ai/en/docs/guide/feature-guide/admin/channel#test-a-channel) |
| LiteLLM | Process readiness and model inference checks answer different operational questions. Its model checks can use real, potentially billable requests. In Oxsum, label connectivity and inference separately and show the scope of each result. [Health checks](https://docs.litellm.ai/docs/proxy/health) |
| Portkey | Its integrated catalog and live provider list are distinct data sources. Oxsum exposes those through separate operator and customer operations, so a caller listing models never needs the provider's credentials. [Models API](https://docs.portkey.ai/docs/api-reference/inference-api/models/models#two-response-modes) |

A dedicated operator inference test is a possible follow-up. Start with one
explicit, audited request to one channel, model and operation, with a bounded
input, output and deadline where the protocol supports those limits. Show
the limits and possible supplier spend before sending. Report inference and
usage parsing separately. The operator test would leave customer balances,
sale terms, routing and permissions untouched. A customer sample exercises
the existing billing path. Real-provider certification covers a defined
provider, model, protocol, scenario and build under
[#740](https://github.com/AllureCurtain/oxsum/issues/740).

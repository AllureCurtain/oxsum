//! The chat page: top up, pick a model and chat, with each turn's billing in real time.
//!
//! The page calls the existing API only, no new endpoints:
//! - Top-up goes to `POST /api/v1/topups` from the browser, with the session cookie
//!   (the login page's pattern — the cookie is `HttpOnly`, so only a real endpoint
//!   call works).
//! - Models and chat go to `GET /v1/models` and streaming `POST /v1/chat/completions`
//!   with an API key. The gateway takes a key only (#17: the session cookie is never
//!   accepted on `/v1`), so the page mints a key through the existing `create_key`
//!   server function and keeps it in the browser's `localStorage` — the user never
//!   handles it by hand.
//! - Billing is the `/ws/billing` socket, shared with the dashboard's holds section.
//! - The settled bill comes from the `get_turn_bill` server function: the settlement
//!   entry's proof bundle plus the charge read out of it. Each settled turn links to
//!   `/verify` with the bundle and the content hash prefilled.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos_router::components::A;
use serde::{Deserialize, Serialize};

use crate::api::{TurnBill, create_key, get_dashboard};

/// One chat turn, as the conversation shows it. Persisted to `localStorage`, so a
/// reload keeps the conversation; the server keeps nothing (docs/product.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Turn {
    /// Stable key for the list; the request id only arrives once the turn starts.
    id: usize,
    model: String,
    user_text: String,
    answer: String,
    request_id: Option<String>,
    failed: Option<String>,
}

/// The live billing state of one turn, keyed by request id. Fed by the billing
/// socket; the settled bill comes from the `get_turn_bill` server function.
#[derive(Debug, Clone, Default)]
struct TurnBilling {
    freeze_minor: Option<i64>,
    output_chars: usize,
    settled: bool,
    bill: Option<TurnBill>,
}

/// The model list's state: it needs a key, and the deployment may serve nothing.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
#[derive(Debug, Clone)]
enum ModelsState {
    /// No key saved yet; the list has not been attempted.
    Idle,
    Loading,
    Ready,
    Failed(String),
}

/// Formats minor units as credits with six decimals, without floating point: money is
/// integers all the way down, including on the way to the screen.
fn credits(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:06}", abs / 1_000_000, abs % 1_000_000)
}

/// Parses a credit amount like `10` or `10.50` into minor units, without floating
/// point: money is integers all the way down, including on the way in.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
fn parse_credits(text: &str) -> Result<i64, ()> {
    let text = text.trim();
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) => (whole, frac),
        None => (text, ""),
    };
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    if frac.len() > 6 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    let mut padded = frac.to_owned();
    while padded.len() < 6 {
        padded.push('0');
    }
    let whole: i64 = whole.parse().map_err(|_| ())?;
    let frac: i64 = padded.parse().map_err(|_| ())?;
    whole
        .checked_mul(1_000_000)
        .and_then(|base| base.checked_add(frac))
        .ok_or(())
}

/// The `/verify` link for a settled turn: the bundle and the content hash travel as
/// query parameters, and `/verify` prefills them.
fn verify_href(bill: &TurnBill) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    // The query-component set: encode everything the query string cannot carry raw.
    const QUERY: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}');
    let bundle = utf8_percent_encode(&bill.bundle_json, QUERY).to_string();
    let hash = utf8_percent_encode(&bill.content_hash, QUERY).to_string();
    format!("/verify?bundle={bundle}&contentHash={hash}")
}

/// The chat page: top up, key, model, conversation.
#[component]
pub fn ChatPage() -> impl IntoView {
    let dashboard = Resource::new(|| (), |_| async { get_dashboard().await });
    let (turns, set_turns) = signal(Vec::<Turn>::new());
    let (billing, set_billing) = signal(HashMap::<String, TurnBilling>::new());
    let (api_key, set_api_key) = signal(Option::<String>::None);
    let (models, set_models) = signal(Vec::<String>::new());
    let (model, set_model) = signal(Option::<String>::None);
    let (models_state, set_models_state) = signal(ModelsState::Idle);
    let (streaming, set_streaming) = signal(false);
    let (input, set_input) = signal(String::new());
    // Bumped by the top-up form; refreshes the balance above.
    let (topup_version, set_topup_version) = signal(0u64);
    Effect::new(move |_| {
        if topup_version.get() > 0 {
            dashboard.refetch();
        }
    });

    // Browser-only setup, once: restore the key and the conversation. The bills of
    // restored turns are re-fetched — their entry ids are derivable from the request
    // ids, so a reload loses nothing but the live socket state.
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        if let Some(key) = browser::load_key() {
            set_api_key.set(Some(key.clone()));
            refresh_models(key, set_models, set_model, set_models_state);
        }
        let history = browser::load_history();
        for turn in &history {
            if let Some(request_id) = &turn.request_id {
                fetch_bill(billing, set_billing, request_id.clone());
            }
        }
        set_turns.set(history);
    });

    // The conversation persists in the browser only; the server keeps nothing.
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        browser::save_history(&turns.get());
    });

    // Live billing for every turn, including ones started elsewhere (another tab, the
    // API): the events carry the request id, so there is no race with the chat call.
    #[cfg(feature = "hydrate")]
    crate::billing_socket::watch(move |event| {
        use crate::billing_socket::BillingEvent;
        match event {
            BillingEvent::Snapshot { holds } => set_billing.update(|map| {
                for hold in holds {
                    map.entry(hold.request_id).or_default().freeze_minor = Some(hold.freeze_minor);
                }
            }),
            BillingEvent::TurnStarted {
                request_id,
                freeze_minor,
                ..
            } => set_billing.update(|map| {
                map.entry(request_id).or_default().freeze_minor = Some(freeze_minor);
            }),
            BillingEvent::TurnProgress {
                request_id,
                output_chars,
            } => set_billing.update(|map| {
                map.entry(request_id).or_default().output_chars = output_chars;
            }),
            BillingEvent::TurnSettled { request_id } => {
                set_billing.update(|map| {
                    map.entry(request_id.clone()).or_default().settled = true;
                });
                fetch_bill(billing, set_billing, request_id);
                // The balance changed: refresh it.
                dashboard.refetch();
            }
        }
    });

    view! {
        <section class="card" aria-label="Chat setup">
            <h1>"Chat"</h1>
            <p class="muted">
                "Top up, pick a model and chat. Each turn's billing shows underneath it, live."
            </p>
            <Suspense fallback=move || view! { <p class="muted">"Loading…"</p> }>
                {move || dashboard.get().map(|result| match result {
                    Ok(data) => view! {
                        <p class="balance">
                            <span class="muted">"Available balance"</span>
                            <strong class="mono">{credits(data.available_minor)}</strong>
                        </p>
                    }.into_any(),
                    Err(_) => view! {
                        <p class="error" role="alert">
                            "Could not load the balance. "
                            <A href="/login">"Log in again"</A> " and retry."
                        </p>
                    }.into_any(),
                })}
            </Suspense>
            <TopUpForm set_topup_version=set_topup_version/>
            <KeySetup
                api_key=api_key
                set_api_key=set_api_key
                set_models=set_models
                set_model=set_model
                set_models_state=set_models_state
            />
            <ModelPicker
                models=models
                model=model
                set_model=set_model
                models_state=models_state
            />
        </section>
        <section class="card" aria-label="Conversation">
            <div class="section-head">
                <h2>"Conversation"</h2>
                <button
                    class="danger"
                    on:click=move |_| {
                        if !streaming.get_untracked() {
                            set_turns.set(Vec::new());
                            set_billing.set(HashMap::new());
                        }
                    }
                    prop:disabled=move || streaming.get() || turns.get().is_empty()
                >
                    "Clear"
                </button>
            </div>
            <div class="chat-log">
                {move || {
                    let current = turns.get();
                    if current.is_empty() {
                        view! { <p class="muted">"No messages yet — say hello."</p> }.into_any()
                    } else {
                        view! {
                            <For
                                each=move || turns.get()
                                key=|turn| turn.id
                                let(turn)
                            >
                                <TurnView turn=turn billing=billing/>
                            </For>
                        }
                            .into_any()
                    }
                }}
            </div>
            <Composer
                input=input
                set_input=set_input
                api_key=api_key
                model=model
                streaming=streaming
                set_streaming=set_streaming
                turns=turns
                set_turns=set_turns
                billing=billing
                set_billing=set_billing
            />
        </section>
    }
}

/// The top-up form: an amount in credits, posted to the existing endpoint with the
/// session cookie.
#[component]
fn TopUpForm(set_topup_version: WriteSignal<u64>) -> impl IntoView {
    let (amount, set_amount) = signal(String::new());
    let (busy, set_busy) = signal(false);
    let (result, set_result) = signal(Option::<Result<String, String>>::None);

    let submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        #[cfg(feature = "hydrate")]
        {
            let amount_minor = match parse_credits(&amount.get_untracked()) {
                Ok(minor) if minor > 0 => minor,
                _ => {
                    set_result.set(Some(Err("Enter an amount like 10 or 10.50.".to_owned())));
                    return;
                }
            };
            set_busy.set(true);
            set_result.set(None);
            leptos::task::spawn_local(async move {
                let outcome = browser::topup(amount_minor).await;
                let ok = outcome.is_ok();
                set_result.set(Some(outcome));
                set_busy.set(false);
                if ok {
                    set_amount.set(String::new());
                    set_topup_version.update(|version| *version += 1);
                }
            });
        }
        #[cfg(not(feature = "hydrate"))]
        let _ = (&set_busy, &set_result, &set_topup_version);
    };

    view! {
        <form class="row" on:submit=submit aria-label="Top up">
            <label>
                "Top up (credits)"
                <input
                    type="text"
                    name="topup-amount"
                    inputmode="decimal"
                    autocomplete="off"
                    placeholder="10"
                    prop:value=move || amount.get()
                    on:input=move |ev| set_amount.set(event_target_value(&ev))
                />
            </label>
            <button type="submit" prop:disabled=move || busy.get()>
                {move || if busy.get() { "Topping up…" } else { "Top up" }}
            </button>
        </form>
        {move || result.get().map(|outcome| match outcome {
            Ok(content_hash) => view! {
                <p class="success" role="status">
                    "Topped up. Keep the content hash to verify the bill later: "
                    <code class="mono">{content_hash}</code>
                </p>
            }.into_any(),
            Err(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
        })}
    }
}

/// The chat key: the gateway takes an API key only, so the page mints one through the
/// existing server function and keeps it in the browser. Pasting a key from the keys
/// page works too.
#[component]
fn KeySetup(
    api_key: ReadSignal<Option<String>>,
    set_api_key: WriteSignal<Option<String>>,
    set_models: WriteSignal<Vec<String>>,
    set_model: WriteSignal<Option<String>>,
    set_models_state: WriteSignal<ModelsState>,
) -> impl IntoView {
    let (pasted, set_pasted) = signal(String::new());
    let (busy, set_busy) = signal(false);
    let (error, set_error) = signal(Option::<String>::None);

    let use_key = move |key: String| {
        #[cfg(feature = "hydrate")]
        browser::save_key(&key);
        set_api_key.set(Some(key.clone()));
        set_pasted.set(String::new());
        set_error.set(None);
        #[cfg(feature = "hydrate")]
        refresh_models(key, set_models, set_model, set_models_state);
        #[cfg(not(feature = "hydrate"))]
        let _ = (key, set_models, set_model, set_models_state);
    };

    let mint = {
        let use_key = use_key.clone();
        move |_| {
            set_busy.set(true);
            set_error.set(None);
            leptos::task::spawn_local(async move {
                // Named "chat": the key's name is what marks these turns in the records.
                match create_key(Some("chat".to_owned())).await {
                    Ok(created) => use_key(created.secret),
                    Err(error) => set_error.set(Some(error.to_string())),
                }
                set_busy.set(false);
            });
        }
    };

    let save_pasted = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let key = pasted.get_untracked().trim().to_owned();
        if !key.is_empty() {
            use_key(key);
        }
    };

    let forget = move |_| {
        #[cfg(feature = "hydrate")]
        browser::forget_key();
        set_api_key.set(None);
        set_models.set(Vec::new());
        set_model.set(None);
        set_models_state.set(ModelsState::Idle);
    };

    view! {
        <h2>"Chat key"</h2>
        {move || match api_key.get() {
            Some(_) => view! {
                <p class="muted">
                    "A chat key is saved in this browser. "
                    <button class="danger" on:click=forget>"Forget it"</button>
                </p>
            }.into_any(),
            None => view! {
                <div>
                    <p class="muted">
                        "The gateway takes an API key, not the login session. Mint one for this browser — you never have to handle it."
                    </p>
                    <form class="row" on:submit=save_pasted aria-label="Use an existing key">
                        <label>
                            "Or paste a key"
                            <input
                                type="text"
                                name="chat-key"
                                autocomplete="off"
                                spellcheck="false"
                                prop:value=move || pasted.get()
                                on:input=move |ev| set_pasted.set(event_target_value(&ev))
                            />
                        </label>
                        <button type="submit">"Use key"</button>
                    </form>
                    <p>
                        <button on:click=mint prop:disabled=move || busy.get()>
                            {move || if busy.get() { "Minting…" } else { "Mint a chat key" }}
                        </button>
                    </p>
                    {move || error.get().map(|message| view! { <p class="error" role="alert">{message}</p> })}
                </div>
            }.into_any(),
        }}
    }
}

/// The model picker, fed by `GET /v1/models` once a key is saved.
#[component]
fn ModelPicker(
    models: ReadSignal<Vec<String>>,
    model: ReadSignal<Option<String>>,
    set_model: WriteSignal<Option<String>>,
    models_state: ReadSignal<ModelsState>,
) -> impl IntoView {
    view! {
        <h2>"Model"</h2>
        {move || match models_state.get() {
            ModelsState::Idle => view! { <p class="muted">"Save a chat key above to list the models."</p> }.into_any(),
            ModelsState::Loading => view! { <p class="muted">"Loading models…"</p> }.into_any(),
            ModelsState::Failed(message) => view! { <p class="error" role="alert">{message}</p> }.into_any(),
            ModelsState::Ready => {
                if models.get().is_empty() {
                    view! { <p class="muted">"This deployment serves no models yet."</p> }.into_any()
                } else {
                    view! {
                        <label>
                            <span class="visually-hidden">"Model"</span>
                            <select
                                name="model"
                                on:change=move |ev| set_model.set(Some(event_target_value(&ev)))
                            >
                                <For each=move || models.get() key=|id| id.clone() let(model_id)>
                                    {
                                        let option_id = model_id.clone();
                                        let shown_id = model_id.clone();
                                        view! {
                                            <option
                                                value=option_id.clone()
                                                selected=move || model.get().as_deref() == Some(option_id.as_str())
                                            >
                                                {shown_id}
                                            </option>
                                        }
                                    }
                                </For>
                            </select>
                        </label>
                    }
                        .into_any()
                }
            }
        }}
    }
}

/// One turn: the user's message, the streamed answer, and the live billing underneath.
///
/// The billing lookup runs in a reactive closure — the turn's bill arrives after the
/// turn itself renders — and the closure returns `AnyView`: the type-erased boundary
/// keeps the nested billing views from overflowing the compiler's type depth.
#[component]
fn TurnView(turn: Turn, billing: ReadSignal<HashMap<String, TurnBilling>>) -> impl IntoView {
    view! {
        <div class="turn user">
            <p class="who">"You"</p>
            <p>{turn.user_text.clone()}</p>
        </div>
        <div class="turn assistant">
            <p class="who">{turn.model.clone()}</p>
            {if turn.answer.is_empty() && turn.failed.is_none() {
                view! { <p class="muted">"…"</p> }.into_any()
            } else {
                view! { <p class="answer">{turn.answer.clone()}</p> }.into_any()
            }}
            {turn.failed.clone().map(|message| view! { <p class="error" role="alert">{message}</p> })}
            {move || {
                let entry = turn
                    .request_id
                    .clone()
                    .and_then(|id| billing.get().get(&id).cloned());
                let Some(entry) = entry else {
                    return ().into_any();
                };
                let live = !entry.settled && entry.bill.is_none();
                view! { <TurnBillingView billing=entry live=live/> }.into_any()
            }}
        </div>
    }
}

/// The billing line under one answer: freeze and streaming progress live, the settled
/// charge with a verification link once the turn is in the log.
#[component]
fn TurnBillingView(billing: TurnBilling, live: bool) -> impl IntoView {
    view! {
        <div class="bill" aria-label="Turn billing">
            {billing
                .freeze_minor
                .map(|freeze| view! {
                    <span class="mono" title="The upper bound frozen before the call">
                        {format!("frozen {}", credits(freeze))}
                    </span>
                })}
            {live.then(|| view! {
                <span class="live" aria-label="Streaming">
                    {format!(" · streaming, {} chars", billing.output_chars)}
                </span>
            })}
            {billing.bill.map(|bill| view! {
                <span>
                    <span class="mono">
                        {format!(
                            "charged {} · kind {} · {} in / {} out tokens · price v{}",
                            credits(bill.charged_minor),
                            bill.kind,
                            bill.input_tokens,
                            bill.output_tokens,
                            bill.price_version,
                        )}
                    </span>
                    " · "
                    <A href=verify_href(&bill)>"Verify this bill"</A>
                </span>
            })}
        </div>
    }
}

/// The composer: the message box and the send button.
#[component]
fn Composer(
    input: ReadSignal<String>,
    set_input: WriteSignal<String>,
    api_key: ReadSignal<Option<String>>,
    model: ReadSignal<Option<String>>,
    streaming: ReadSignal<bool>,
    set_streaming: WriteSignal<bool>,
    turns: ReadSignal<Vec<Turn>>,
    set_turns: WriteSignal<Vec<Turn>>,
    billing: ReadSignal<HashMap<String, TurnBilling>>,
    set_billing: WriteSignal<HashMap<String, TurnBilling>>,
) -> impl IntoView {
    let send = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        let text = input.get_untracked().trim().to_owned();
        if text.is_empty() || streaming.get_untracked() {
            return;
        }
        let (Some(key), Some(model_name)) = (api_key.get_untracked(), model.get_untracked()) else {
            return;
        };
        set_input.set(String::new());
        // The history the model sees: the turns so far, then this message.
        let mut history: Vec<(String, String)> = turns
            .get_untracked()
            .iter()
            .flat_map(|turn| {
                let mut messages = vec![("user".to_owned(), turn.user_text.clone())];
                if !turn.answer.is_empty() {
                    messages.push(("assistant".to_owned(), turn.answer.clone()));
                }
                messages
            })
            .collect();
        history.push(("user".to_owned(), text.clone()));
        let index = turns.with_untracked(|list| list.len());
        set_turns.update(|list| {
            list.push(Turn {
                id: index,
                model: model_name.clone(),
                user_text: text,
                answer: String::new(),
                request_id: None,
                failed: None,
            });
        });
        set_streaming.set(true);
        #[cfg(feature = "hydrate")]
        {
            let set_turns = set_turns;
            leptos::task::spawn_local(async move {
                let update_request_id = move |request_id: String| {
                    set_turns.update(|list| {
                        if let Some(turn) = list.get_mut(index) {
                            turn.request_id = Some(request_id);
                        }
                    });
                };
                let append_token = move |token: &str| {
                    let token = token.to_owned();
                    set_turns.update(|list| {
                        if let Some(turn) = list.get_mut(index) {
                            turn.answer.push_str(&token);
                        }
                    });
                };
                let outcome =
                    browser::chat(&key, &model_name, &history, update_request_id, append_token)
                        .await;
                // The stream only closes after the turn settles, so the bill is in the
                // log now — unless the socket already fetched it.
                if let Some(request_id) = turns
                    .with_untracked(|list| list.get(index).and_then(|turn| turn.request_id.clone()))
                {
                    fetch_bill(billing, set_billing, request_id);
                }
                if let Err(message) = outcome {
                    set_turns.update(|list| {
                        if let Some(turn) = list.get_mut(index) {
                            turn.failed = Some(message);
                        }
                    });
                }
                set_streaming.set(false);
            });
        }
        // SSR only emits the composer; sending runs in the browser.
        #[cfg(not(feature = "hydrate"))]
        let _ = (key, billing, set_turns, set_billing, set_streaming);
    };

    let ready = move || {
        api_key.get().is_some()
            && model.get().is_some()
            && !streaming.get()
            && !input.get().trim().is_empty()
    };

    view! {
        <form class="composer" on:submit=send aria-label="Send a message">
            <label>
                <span class="visually-hidden">"Message"</span>
                <textarea
                    name="message"
                    rows="3"
                    placeholder="Ask something…"
                    prop:value=move || input.get()
                    on:input=move |ev| set_input.set(event_target_value(&ev))
                />
            </label>
            <button type="submit" prop:disabled=move || !ready()>
                {move || if streaming.get() { "Sending…" } else { "Send" }}
            </button>
        </form>
        {move || {
            if api_key.get().is_none() {
                view! { <p class="muted">"Save a chat key above before chatting."</p> }.into_any()
            } else if model.get().is_none() {
                view! { <p class="muted">"Pick a model above before chatting."</p> }.into_any()
            } else {
                ().into_any()
            }
        }}
    }
}

/// Reloads the model list for a key.
#[cfg(feature = "hydrate")]
fn refresh_models(
    key: String,
    set_models: WriteSignal<Vec<String>>,
    set_model: WriteSignal<Option<String>>,
    set_models_state: WriteSignal<ModelsState>,
) {
    set_models_state.set(ModelsState::Loading);
    leptos::task::spawn_local(async move {
        match browser::models(&key).await {
            Ok(ids) => {
                set_model.set(ids.first().cloned());
                set_models.set(ids);
                set_models_state.set(ModelsState::Ready);
            }
            Err(message) => set_models_state.set(ModelsState::Failed(message)),
        }
    });
}

/// Fetches a settled turn's bill from the server function, unless it is already
/// known: the settled socket event and the post-stream fallback both call this.
#[cfg(feature = "hydrate")]
fn fetch_bill(
    billing: ReadSignal<HashMap<String, TurnBilling>>,
    set_billing: WriteSignal<HashMap<String, TurnBilling>>,
    request_id: String,
) {
    if billing.with_untracked(|map| {
        map.get(&request_id)
            .is_some_and(|entry| entry.bill.is_some())
    }) {
        return;
    }
    leptos::task::spawn_local(async move {
        if let Ok(bill) = browser::settlement_bill(&request_id).await {
            set_billing.update(|map| {
                let entry = map.entry(request_id).or_default();
                entry.bill = Some(bill);
                entry.settled = true;
            });
        }
    });
}

/// Browser-only calls: the existing API, `fetch`ed directly.
#[cfg(feature = "hydrate")]
mod browser {
    use gloo_net::http::Request;
    use serde::Serialize;
    use uuid::Uuid;
    use wasm_bindgen::JsCast;
    use wasm_bindgen_futures::JsFuture;
    use web_sys::{ReadableStreamDefaultReader, TextDecodeOptions, TextDecoder};

    use crate::api::get_turn_bill;

    const KEY_STORAGE: &str = "oxsum-chat-key";
    const HISTORY_STORAGE: &str = "oxsum-chat-history";

    fn storage() -> Option<web_sys::Storage> {
        web_sys::window()?.local_storage().ok()?
    }

    pub fn load_key() -> Option<String> {
        storage()?.get_item(KEY_STORAGE).ok()?
    }

    pub fn save_key(key: &str) {
        if let Some(storage) = storage() {
            let _ = storage.set_item(KEY_STORAGE, key);
        }
    }

    pub fn forget_key() {
        if let Some(storage) = storage() {
            let _ = storage.remove_item(KEY_STORAGE);
        }
    }

    pub fn load_history() -> Vec<super::Turn> {
        storage()
            .and_then(|storage| storage.get_item(HISTORY_STORAGE).ok())
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    pub fn save_history(turns: &[super::Turn]) {
        if let Some(storage) = storage() {
            if let Ok(json) = serde_json::to_string(turns) {
                let _ = storage.set_item(HISTORY_STORAGE, &json);
            }
        }
    }

    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct TopUpBody<'a> {
        idempotency_key: &'a str,
        amount_minor: i64,
    }

    /// Tops up through the existing endpoint. The session cookie authenticates the
    /// call — the browser sends it on its own — and the receipt's content hash is
    /// what the bill is verified against later.
    pub async fn topup(amount_minor: i64) -> Result<String, String> {
        let idempotency_key = Uuid::new_v4().to_string();
        let response = Request::post("/api/v1/topups")
            .json(&TopUpBody {
                idempotency_key: &idempotency_key,
                amount_minor,
            })
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if !response.ok() {
            return Err(format!(
                "top-up failed ({}); the amount was not charged",
                response.status()
            ));
        }
        response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| {
                body.pointer("/data/contentHash")
                    .and_then(|hash| hash.as_str())
                    .map(str::to_owned)
            })
            .ok_or_else(|| "the server answered strangely".to_owned())
    }

    /// The models the deployment serves, in the gateway's own list shape.
    pub async fn models(api_key: &str) -> Result<Vec<String>, String> {
        let response = Request::get("/v1/models")
            .header("authorization", &format!("Bearer {api_key}"))
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        match response.status() {
            200 => {}
            401 => return Err("that key is not valid — the gateway refused it".to_owned()),
            status => return Err(format!("the model list failed ({status})")),
        }
        response
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|body| {
                body.pointer("/data")?.as_array().map(|models| {
                    models
                        .iter()
                        .filter_map(|model| model.get("id"))
                        .filter_map(|id| id.as_str())
                        .map(str::to_owned)
                        .collect()
                })
            })
            .ok_or_else(|| "the model list did not parse".to_owned())
    }

    #[derive(Serialize)]
    struct ChatMessage<'a> {
        role: &'a str,
        content: &'a str,
    }

    #[derive(Serialize)]
    struct ChatBody<'a> {
        model: &'a str,
        messages: Vec<ChatMessage<'a>>,
        stream: bool,
    }

    /// One streaming chat turn through the gateway. The request id arrives on the
    /// response headers, before the first frame; the answer arrives token by token.
    pub async fn chat(
        api_key: &str,
        model: &str,
        history: &[(String, String)],
        mut on_request_id: impl FnMut(String),
        mut on_token: impl FnMut(&str),
    ) -> Result<(), String> {
        let body = ChatBody {
            model,
            messages: history
                .iter()
                .map(|(role, content)| ChatMessage { role, content })
                .collect(),
            stream: true,
        };
        let response = Request::post("/v1/chat/completions")
            .header("authorization", &format!("Bearer {api_key}"))
            .json(&body)
            .map_err(|_| "could not build the request".to_owned())?
            .send()
            .await
            .map_err(|_| "the server could not be reached".to_owned())?;
        if !response.ok() {
            return Err(gateway_error(response).await);
        }
        let request_id = response
            .headers()
            .get("x-oxsum-request-id")
            .ok_or_else(|| "the response carried no request id".to_owned())?;
        on_request_id(request_id);
        let stream = response
            .body()
            .ok_or_else(|| "the response had no body".to_owned())?;
        pump_sse(stream, &mut on_token).await
    }

    /// The gateway's refusal, in its own OpenAI error shape.
    async fn gateway_error(response: gloo_net::http::Response) -> String {
        let status = response.status();
        let message = response
            .text()
            .await
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
            .and_then(|body| {
                body.pointer("/error/message")
                    .and_then(|message| message.as_str())
                    .map(str::to_owned)
            });
        match message {
            Some(message) => format!("the gateway refused ({status}): {message}"),
            None => format!("the gateway refused ({status})"),
        }
    }

    /// Reads the SSE stream, calling `on_token` for every content delta.
    async fn pump_sse(
        stream: web_sys::ReadableStream,
        on_token: &mut impl FnMut(&str),
    ) -> Result<(), String> {
        let reader: ReadableStreamDefaultReader = stream.get_reader().unchecked_into();
        let decoder =
            TextDecoder::new().map_err(|_| "the text decoder would not start".to_owned())?;
        let mut decode_options = TextDecodeOptions::new();
        decode_options.stream(true);
        let mut pending = String::new();
        loop {
            let chunk = JsFuture::from(reader.read())
                .await
                .map_err(|_| "the stream broke".to_owned())?;
            let done = js_sys::Reflect::get(&chunk, &"done".into())
                .ok()
                .and_then(|value| value.as_bool())
                .unwrap_or(true);
            let value = js_sys::Reflect::get(&chunk, &"value".into())
                .map_err(|_| "the stream broke".to_owned())?;
            if !value.is_undefined() {
                let bytes = js_sys::Uint8Array::new(&value);
                let mut raw = bytes.to_vec();
                let text = decoder
                    .decode_with_u8_array_and_options(&mut raw, &decode_options)
                    .map_err(|_| "the stream broke".to_owned())?;
                pending.push_str(&text);
                while let Some(end) = pending.find('\n') {
                    let line = pending[..end].to_owned();
                    pending = pending[end + 1..].to_owned();
                    if sse_done(&line) {
                        return Ok(());
                    }
                    sse_token(&line, on_token);
                }
            }
            if done {
                // A tail without a trailing newline still counts.
                let tail = std::mem::take(&mut pending);
                if !tail.trim().is_empty() {
                    if sse_done(&tail) {
                        return Ok(());
                    }
                    sse_token(&tail, on_token);
                }
                return Ok(());
            }
        }
    }

    /// `true` when the line ends the stream.
    fn sse_done(line: &str) -> bool {
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            return false;
        }
        line.strip_prefix("data:")
            .is_some_and(|data| data.trim() == "[DONE]")
    }

    /// Appends one frame's content delta, when it carries any.
    fn sse_token(line: &str, on_token: &mut impl FnMut(&str)) {
        let Some(data) = line.trim().strip_prefix("data:") else {
            return;
        };
        let data = data.trim();
        if data == "[DONE]" {
            return;
        }
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(data) else {
            return;
        };
        if let Some(content) = frame
            .pointer("/choices/0/delta/content")
            .and_then(|value| value.as_str())
        {
            on_token(content);
        }
    }

    /// The settled bill for a turn, through the `get_turn_bill` server function:
    /// the proof bundle plus the charge, with the content hash the server recomputed
    /// from the entry. `None` — the turn has not settled yet — is the caller retrying,
    /// not an error the page shows.
    pub async fn settlement_bill(request_id: &str) -> Result<super::TurnBill, String> {
        get_turn_bill(request_id.to_owned())
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "the turn has not settled yet".to_owned())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{TurnBill, parse_credits, verify_href};

    fn bill() -> TurnBill {
        TurnBill {
            request_id: "req-1".to_owned(),
            model: "chat-e2e".to_owned(),
            charged_minor: 34,
            kind: "usage".to_owned(),
            input_tokens: 23,
            output_tokens: 11,
            price_version: 3,
            freeze_minor: 118,
            content_hash: "ab".repeat(32),
            bundle_json: r#"{"entry": {"id": "x"}}"#.to_owned(),
        }
    }

    #[test]
    fn parse_credits_handles_whole_and_fractional_amounts() {
        assert_eq!(parse_credits("10").unwrap(), 10_000_000);
        assert_eq!(parse_credits("10.5").unwrap(), 10_500_000);
        assert_eq!(parse_credits("0.000001").unwrap(), 1);
        assert_eq!(parse_credits("  3.25 ").unwrap(), 3_250_000);
    }

    #[test]
    fn parse_credits_refuses_non_amounts() {
        for bad in ["", "abc", "10.1234567", "-5", "1e3", ".5", " "] {
            assert!(parse_credits(bad).is_err(), "{bad:?} should not parse");
        }
        // A trailing dot is unambiguous: "10." is ten credits.
        assert_eq!(parse_credits("10.").unwrap(), 10_000_000);
    }

    #[test]
    fn verify_link_carries_bundle_and_hash_as_query_params() {
        let href = verify_href(&bill());
        assert!(href.starts_with("/verify?bundle="), "{href}");
        assert!(href.contains("contentHash="), "{href}");
        // The JSON braces do not travel raw.
        assert!(!href.contains('{'), "{href}");
        // …but the encoded bundle decodes back to the bundle.
        let encoded = href
            .strip_prefix("/verify?bundle=")
            .unwrap()
            .split("&contentHash=")
            .next()
            .unwrap()
            .replace("%7B", "{")
            .replace("%7D", "}")
            .replace("%22", "\"")
            .replace("%3A", ":")
            .replace("%20", " ");
        assert_eq!(encoded, r#"{"entry": {"id": "x"}}"#);
    }
}

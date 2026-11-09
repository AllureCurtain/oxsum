/** Lossless JSON field selection and externally retained verification scope. */
export function inspect(raw) {
  JSON.parse(raw); // Syntax validation only; original number tokens stay in raw.
  const spans = new Map();
  let at = 0;
  const space = () => { while (/\s/.test(raw[at] ?? '') && at < raw.length) at += 1; };
  const string = () => {
    const start = at++;
    while (at < raw.length) {
      if (raw[at] === '\\') at += 2;
      else if (raw[at++] === '"') break;
    }
    return JSON.parse(raw.slice(start, at));
  };
  const value = (path) => {
    space();
    const start = at;
    if (raw[at] === '{') {
      at += 1; space();
      const keys = new Set();
      while (raw[at] !== '}') {
        const key = string();
        if (keys.has(key)) throw new Error('Duplicate JSON field');
        keys.add(key); space(); at += 1;
        value([...path, key]); space();
        if (raw[at] === '}') break;
        at += 1; space();
      }
      at += 1;
    } else if (raw[at] === '[') {
      at += 1; space();
      let index = 0;
      while (raw[at] !== ']') {
        value([...path, index++]); space();
        if (raw[at] === ']') break;
        at += 1; space();
      }
      at += 1;
    } else if (raw[at] === '"') string();
    else { while (at < raw.length && !/[\s,}\]]/.test(raw[at])) at += 1; }
    spans.set(JSON.stringify(path), [start, at]);
  };
  value([]);
  const token = (path) => {
    const span = spans.get(JSON.stringify(path));
    if (!span) throw new Error('Missing required field: '+path.join('.'));
    return raw.slice(...span);
  };
  return {
    token,
    get: (path) => JSON.parse(token(path)),
    has: (path) => spans.has(JSON.stringify(path)),
    replace(path, replacement) {
      const [start, end] = spans.get(JSON.stringify(path)) ?? [];
      if (start === undefined) throw new Error('Mutation field is absent');
      JSON.parse(replacement);
      if (raw.slice(start, end) === replacement) throw new Error('Mutation did not change the input');
      return raw.slice(0, start)+replacement+raw.slice(end);
    },
  };
}

function require(condition, message) {
  if (!condition) throw new Error(message);
}

function sameHead(a, b) {
  require(a.get(['root']) === b.get(['root']) && a.token(['size']) === b.token(['size']),
    'Retained head differs from exported head');
}

function bindingKey(bundle, binding, keyRaw) {
  const key = inspect(keyRaw);
  const publicKey = key.get(['publicKey']);
  const hex = /^[0-9a-f]{64}$/i.test(publicKey)
    ? publicKey.toLowerCase() : Buffer.from(publicKey, 'base64').toString('hex');
  require(bundle.get([binding, 'keyName']) === key.get(['keyName'])
    && bundle.get([binding, 'keyHash']) === key.get(['keyHash'])
    && bundle.get([binding, 'publicKey']).toLowerCase() === hex,
  'Catalog signature does not use the retained operator key');
}

export async function scopedBill(raw, manifestRaw, headRaw, keyRaw, api) {
  const bundle = inspect(raw), manifest = inspect(manifestRaw), head = inspect(headRaw);
  require(bundle.get(['entry','id']) === manifest.get(['entryId']), 'Wrong retained entry identity');
  sameHead(inspect(bundle.token(['head'])), head);
  require(manifest.get(['headRoot']) === head.get(['root']) && manifest.token(['headSize']) === head.token(['size']),
    'Manifest head differs');
  require(head.get(['origin']) === manifest.get(['origin']), 'Wrong retained ledger origin');
  const bill = await api.verifyBill(raw, manifest.get(['contentHash']));
  require(bill.ok, bill.detail ?? bill.verdict ?? 'Bill verification failed');
  // verifyBill reports price/sale failures in prose without changing its
  // inclusion/arithmetic verdict. Require separate catalog verdicts here.
  const description = inspect(bundle.get(['entry', 'description']));
  const salePriced = description.has(['sale']) && description.get(['sale']) !== null;
  for (const binding of ['price', 'sale']) {
    const present = bundle.has([binding]) && bundle.get([binding]) !== null;
    const required = binding === 'sale' ? salePriced : !salePriced;
    require(!required || present, 'Missing required '+binding+' binding');
    if (!present) continue;
    const checked = await api[binding === 'price' ? 'verifyPriceHead' : 'verifySaleHead'](raw);
    require(checked.ok, checked.detail ?? 'Catalog binding failed');
    bindingKey(bundle, binding, keyRaw);
  }
  if (manifest.has(['priceVersion'])) require(description.token(['priceVersion']) === manifest.token(['priceVersion']), 'Wrong retained price version');
  if (manifest.has(['sale'])) {
    require(salePriced, 'Missing retained sale terms');
    const row = inspect(bundle.get(['sale', 'row']));
    require(row.get(['bookId']) === manifest.get(['sale', 'bookId'])
      && row.token(['version']) === manifest.token(['sale', 'version']), 'Wrong retained sale version');
  }
  const signed = await api.verifySignedHead(head.get(['note']), bundle.token(['head']), manifest.get(['origin']), keyRaw);
  require(signed.ok, signed.detail ?? 'Head signature failed');
  return bill;
}

export async function scopedEvidence(raw, manifestRaw, keyRaw, api) {
  const bundle = inspect(raw), manifest = inspect(manifestRaw);
  require(bundle.get(['bundleId']) === manifest.get(['bundleId']), 'Wrong retained bundle identity');
  require(bundle.get(['organizationId']) === manifest.get(['organizationId']), 'Wrong retained customer');
  if (manifest.has(['manifestDigest'])) require(bundle.get(['manifestDigest']) === manifest.get(['manifestDigest']), 'Wrong retained manifest digest');
  const checked = await api.verifyEvidenceBundle(raw, manifest.get(['organizationId']), keyRaw);
  require(checked.ok, checked.detail ?? 'Evidence verification failed');
  // A bundle's self-declared signed origin is insufficient. The SDK's evidence
  // wrapper derives that origin from its note; bind the separately retained one.
  require(bundle.has(['note']) && typeof bundle.get(['note']) === 'string' && bundle.get(['note']).length > 0,
    'Unsigned evidence cannot satisfy this retained-key exercise');
  const signed = await api.verifySignedHead(bundle.get(['note']), bundle.token(['head']), manifest.get(['origin']), keyRaw);
  require(signed.ok, signed.detail ?? 'Evidence signed origin failed');
  return checked;
}

export async function scopedConsistency(raw, heldRaw, headRaw, origin, keyRaw, api) {
  const response = inspect(raw), head = inspect(headRaw);
  sameHead(inspect(response.token(['head'])), head);
  const checked = await api.verifyConsistency(raw, heldRaw, origin, keyRaw);
  require(checked.ok, checked.detail ?? 'Consistency verification failed');
  return checked;
}

export async function scopedWitnesses(notes, pins, origin, headRaw, api) {
  require(notes.length > 0, 'No cosignatures to verify');
  require(new Set(notes.map((note) => note.witnessUrl)).size === notes.length,
    'Duplicate witness notes');
  require(new Set(pins.map((pin) => pin.witnessUrl)).size === pins.length,
    'Duplicate witness pins');
  const checked = await api.verifyWitnesses(JSON.stringify(notes), JSON.stringify(pins), origin, headRaw);
  require(Array.isArray(checked) && checked.length === notes.length,
    'Witness verifier returned incomplete results');
  require(checked.every((row, i) => row.witnessUrl === notes[i].witnessUrl && row.ok === true),
    checked.map((row) => row.witnessUrl+': '+row.detail).join('; '));
  return { ok: true, detail: checked.map((row) => row.witnessUrl+': '+row.detail).join('; ') };
}

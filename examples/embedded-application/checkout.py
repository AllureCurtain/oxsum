#!/usr/bin/env python3
"""Serve one retained customer's checkout on a private loopback URL."""
import argparse
import html
import http.server
import json
import os
import secrets
import urllib.parse
from app import Application, data_bytes


def document(value, nonce):
    order = value["order"]
    status = html.escape(order["status"])
    if order["status"] == "fulfilled":
        return f"<!doctype html><html lang='en'><meta charset='utf-8'><title>Payment complete</title><h1>Payment fulfilled</h1><p>{html.escape(order['orderId'])}</p></html>".encode()
    if order["status"] not in ("created", "provider_pending"):
        raise ValueError("this order is not available for checkout; read current state")
    title = "<h1>Complete payment</h1><p>Payment status: " + status + ". Return here and refresh to check the authoritative result.</p>"
    if value.get("checkoutUrl"):
        url = urllib.parse.urlsplit(value["checkoutUrl"])
        if url.scheme != "https" or not url.hostname or url.username or url.password:
            raise ValueError("hosted checkout URL must use HTTPS without credentials")
        body = title + '<a rel="noreferrer" href="' + html.escape(value["checkoutUrl"], quote=True) + '">Open payment provider</a>'
    else:
        url = urllib.parse.urlsplit(value.get("sdkUrl") or "")
        if url.scheme != "https" or not url.hostname or url.username or url.password or url.query or url.fragment:
            raise ValueError("checkout SDK requires the owner-configured HTTPS URL")
        if not value.get("clientSecret") or not value.get("publishableKey"):
            raise ValueError("checkout handles unavailable; recover the original order")
        # Escape literal script delimiters from all retained provider data.
        config = json.dumps({"clientSecret": value["clientSecret"], "publishableKey": value["publishableKey"], "sdk": value["sdkUrl"].rstrip("/") + "/HyperLoader.js"}).replace("<", "\\u003c").replace(">", "\\u003e").replace("&", "\\u0026")
        body = title + '<div id="payment"></div><p id="status" role="status">Loading payment form…</p>'
        body += '<script nonce="' + nonce + '">const c=' + config + ';const s=document.createElement("script");s.src=c.sdk;s.onload=()=>{try{const h=window.Hyper(c.publishableKey,{});h.elements({clientSecret:c.clientSecret}).create("payment",{}).mount("#payment");document.querySelector("#status").textContent="Use the payment provider form. Completion is checked by the billing server.";}catch{document.querySelector("#status").textContent="Payment form unavailable. Recover the same order; no payment is assumed.";}};s.onerror=()=>document.querySelector("#status").textContent="Payment provider unavailable.";document.head.appendChild(s);</script>'
    return ("<!doctype html><html lang='en'><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'><title>Application checkout</title><main>" + body + "</main></html>").encode()


def handler(app, customer, operation, capability):
    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            if self.path != "/" + capability:
                self.send_error(404)
                return
            try:
                with app.serialized():
                    row = app.db.execute("SELECT response FROM operations WHERE id=? AND customer=? AND kind IN ('checkout','resume')", (operation, customer)).fetchone()
                    if row is None or row[0] is None:
                        raise ValueError("recover the exact checkout operation first")
                    value = json.loads(data_bytes(row[0]))
                    order = app.read("/api/v1/integration/customers/" + app.org(customer) + "/orders/" + value["order"]["orderId"])
                    value["order"] = order
                    content = document(value, secrets.token_hex(16))
            except Exception:
                self.send_error(409, "Checkout unavailable; inspect the original operation")
                return
            self.send_response(200)
            self.send_header("content-type", "text/html; charset=utf-8")
            self.send_header("cache-control", "private, no-store")
            self.send_header("referrer-policy", "no-referrer")
            self.send_header("x-content-type-options", "nosniff")
            self.send_header("x-frame-options", "DENY")
            self.send_header("content-length", str(len(content)))
            self.end_headers()
            self.wfile.write(content)
    return Handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True)
    parser.add_argument("--application-id", required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:3000")
    parser.add_argument("--customer", required=True)
    parser.add_argument("--operation", required=True)
    args = parser.parse_args()
    os.umask(0o077)
    app = Application(args.database, args.base_url, args.application_id, os.environ.get("OXSUM_APPLICATION_CREDENTIAL", ""))
    capability = secrets.token_urlsafe(32)
    server = http.server.HTTPServer(("127.0.0.1", 0), handler(app, args.customer, args.operation, capability))
    print(f"Private checkout URL: http://127.0.0.1:{server.server_port}/{capability}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close(); app.close()


if __name__ == "__main__": main()

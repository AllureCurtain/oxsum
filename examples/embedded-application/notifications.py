#!/usr/bin/env python3
"""Receive signed signals into this application's separately bound SQLite store."""
import argparse
import http.server
import os
from pathlib import Path
import sqlite3
import sys
from app import Application
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "application-notifications"))
from receiver import Receiver, handler


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database", required=True)
    parser.add_argument("--application-id", required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:3000")
    parser.add_argument("--port", type=int, default=8081)
    args = parser.parse_args()
    os.umask(0o077)
    credential = os.environ.get("OXSUM_APPLICATION_CREDENTIAL", "")
    secret = os.environ.get("OXSUM_NOTIFICATION_SECRET", "")
    if not secret.startswith("whsec-"):
        parser.error("supply the one-time notification secret in the environment")
    # Refuse accidental use of a store bound to another deployment/application.
    app = Application(args.database, args.base_url, args.application_id, credential)
    app.close()
    receiver = Receiver(args.database, args.application_id, secret, args.base_url, credential)
    server = http.server.HTTPServer(("127.0.0.1", args.port), handler(receiver))
    server.timeout = 1
    print("Local application notification receiver ready", flush=True)
    try:
        while True:
            server.handle_request()
            try:
                receiver.reconcile()
            except (ValueError, OSError, sqlite3.Error):
                pass  # Durable intake remains pending until current authority/state permits a read.
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
        receiver.db.close()


if __name__ == "__main__":
    main()

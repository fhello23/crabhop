#!/usr/bin/env python3
"""Read-only Caddy log-redaction smoke test; requires its container ID."""

import base64
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import urllib.error
import urllib.request
import uuid

base_url = sys.argv[1].rstrip("/")
container = os.environ["SMOKE_CADDY_CONTAINER"]
marker = "private-log-probe-" + uuid.uuid4().hex
credentials = base64.b64encode(
    (os.environ.get("ADMIN_USERNAME", "admin") + ":" + os.environ["ADMIN_PASSWORD"]).encode()
).decode()

for path, authenticated, expected in [
    ("/" + marker, False, 404),
    ("/admin/links/" + marker, False, 401),
    ("/%61dmin/links/" + marker, False, 401),
    ("/api/v1/links/" + marker, True, 404),
]:
    headers = {
        "Referer": base_url + "/" + marker,
        "User-Agent": marker,
        "X-Crabhop-Proxy-Token": marker,
    }
    if authenticated:
        headers["Authorization"] = "Basic " + credentials
    request = urllib.request.Request(base_url + path + "?q=" + marker, headers=headers)
    try:
        response = urllib.request.urlopen(request, timeout=10)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        assert response.status == expected, (path, response.status, expected)
        response.read()

access = subprocess.check_output(
    ["docker", "exec", container, "cat", "/var/log/caddy/access.log"], text=True
)
runtime = subprocess.check_output(["docker", "logs", container], stderr=subprocess.STDOUT, text=True)
assert marker not in access, "share path, query, or header leaked in Caddy access log"
assert marker not in runtime, "share path, query, or header leaked in Caddy runtime log"
rows = [json.loads(line) for line in access.splitlines() if line.strip()]
assert any(
    row.get("status") == 401
    and row.get("request", {}).get("uri") == "/admin/[redacted]"
    and row["request"].get("client_ip")
    for row in rows
), "redaction must retain management 401s and client_ip for fail2ban"
assert any(
    row.get("status") == 404 and row.get("request", {}).get("uri") == "/[redacted]"
    for row in rows
), "public requests must be logged with a redacted URI"
filter_path = Path(__file__).resolve().parent.parent / "deploy/fail2ban/filter.d/crabhop-caddy.conf"
pattern = next(line.partition("=")[2].strip() for line in filter_path.read_text().splitlines()
               if line.startswith("failregex ="))
for line in access.splitlines():
    row = json.loads(line)
    if row.get("status") == 401:
        # Use the observed address where fail2ban expands its <HOST> token.
        assert re.search(pattern.replace("<HOST>", re.escape(row["request"]["client_ip"])), line), \
            "the shipped fail2ban filter must match every authentication failure"
print("Caddy log privacy smoke test passed; fail2ban fields retained")

#!/usr/bin/env python3
"""Disposable Postgres, PostgREST and PowerSync for the row model.

Starts the three services the client talks to, applies
`supabase/migrations/20260912000000_row_model.sql` to an empty database, checks
the row-level security with three minted users, and then runs whichever Rust
tests were named on the command line against the live stack.

Older migrations are not applied: they assume the deployed baseline the row
model replaces, and the row model migration is self-sufficient by design.
"""

import argparse
import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
MIGRATION = REPO / "supabase/migrations/20260912000000_row_model.sql"
SYNC_RULES = REPO / "deploy/sync-rules.yaml"

PREFIX = "luma-rows-"
NETWORK = PREFIX + "net"
PG = PREFIX + "postgres"
REST = PREFIX + "postgrest"
PS = PREFIX + "powersync"

POSTGRES_IMAGE = "postgres:16"
POSTGREST_IMAGE = "postgrest/postgrest:v12.2.3"
POWERSYNC_IMAGE = "journeyapps/powersync-service:latest"

DB = "luma_rows"
PASSWORD = "row-model-test-only"
SECRET = "luma-row-model-test-secret-not-for-production"
PG_PORT = 55450
REST_PORT = 55451
PS_PORT = 55452

PG_URI = f"postgres://postgres:{PASSWORD}@127.0.0.1:{PG_PORT}/{DB}"
REST_URL = f"http://127.0.0.1:{REST_PORT}"
PS_URL = f"http://127.0.0.1:{PS_PORT}"

ENVIRONMENT = {
    "LUMA_TEST_POWERSYNC_URL": PS_URL,
    "LUMA_TEST_POSTGREST_URL": REST_URL,
    "LUMA_TEST_JWT_SECRET": SECRET,
    "LUMA_TEST_PG_URI": PG_URI,
}

# A minimal stand-in for the Supabase auth schema: enough for `auth.uid()`
# defaults, the `auth.users` foreign keys and PostgREST's role switching.
BOOTSTRAP = f"""
create role anon nologin noinherit;
create role authenticated nologin noinherit;
create role authenticator login noinherit password '{PASSWORD}';
grant anon, authenticated to authenticator;

create schema auth;
create table auth.users (id uuid primary key);

create function auth.uid() returns uuid
language sql
stable
as $$
    select nullif(current_setting('request.jwt.claims', true)::jsonb ->> 'sub', '')::uuid;
$$;

grant usage on schema auth to anon, authenticated;
grant select on auth.users to anon, authenticated;
grant usage on schema public to anon, authenticated;
"""

# The migration creates powersync_role without a usable password, as the
# deployed project expects the operator to set one.
REPLICATION = f"""
alter role powersync_role with password '{PASSWORD}';
"""


def docker(*args, **kwargs):
    return subprocess.run(
        ["docker", *args], check=True, text=True, capture_output=True, **kwargs
    ).stdout


def psql(sql, database=DB, user="postgres"):
    return subprocess.run(
        ["docker", "exec", "-i", PG, "psql", "-U", user, "-d", database,
         "-v", "ON_ERROR_STOP=1", "-q"],
        input=sql, check=True, text=True, capture_output=True,
    ).stdout


def mint_jwt(user_id, role="authenticated", lifetime=3600):
    """A Supabase-shaped HS256 token for `user_id`."""
    encode = lambda value: base64.urlsafe_b64encode(
        json.dumps(value, separators=(",", ":")).encode()
    ).decode().rstrip("=")
    issued = int(time.time())
    header = encode({"alg": "HS256", "kid": "luma-row-model-test", "typ": "JWT"})
    claims = encode({
        "sub": user_id,
        "role": role,
        "aud": "authenticated",
        "iat": issued,
        "exp": issued + lifetime,
    })
    body = f"{header}.{claims}"
    signature = hmac.new(SECRET.encode(), body.encode(), hashlib.sha256).digest()
    return body + "." + base64.urlsafe_b64encode(signature).decode().rstrip("=")


def rest(token, method, path, body=None, prefer=None):
    """One PostgREST call as the holder of `token`."""
    headers = {"Authorization": f"Bearer {token}", "Accept": "application/json"}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    if prefer:
        headers["Prefer"] = prefer
    request = urllib.request.Request(REST_URL + path, data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(request) as response:
            payload = response.read().decode()
            return response.status, json.loads(payload) if payload.strip() else None
    except urllib.error.HTTPError as error:
        payload = error.read().decode()
        try:
            return error.code, json.loads(payload)
        except json.JSONDecodeError:
            return error.code, payload


def stream(token, timeout=30):
    """One PowerSync checkpoint over the HTTP stream: table -> row count."""
    request = urllib.request.Request(
        PS_URL + "/sync/stream",
        data=json.dumps({"buckets": [], "include_checksum": True, "raw_data": True}).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST")
    seen = {}
    with urllib.request.urlopen(request, timeout=timeout) as response:
        for line in response:
            if not line.strip():
                continue
            message = json.loads(line)
            for operation in message.get("data", {}).get("data", []):
                table = operation.get("object_type")
                if table:
                    seen[table] = seen.get(table, 0) + 1
            if "checkpoint_complete" in message:
                break
    return seen


def wait(label, probe, attempts=300, delay=0.3):
    for _ in range(attempts):
        try:
            if probe():
                return
        except Exception:
            pass
        time.sleep(delay)
    raise RuntimeError(f"{label} did not become ready")


def refuse_existing():
    for name in [PG, REST, PS]:
        if subprocess.run(["docker", "container", "inspect", name], capture_output=True).returncode == 0:
            raise RuntimeError(f"Container already exists: {name}. This runner will not remove it.")


def start(containers):
    docker("network", "create", NETWORK)

    print("postgres", flush=True)
    docker("run", "-d", "--name", PG, "--network", NETWORK,
           "--tmpfs", "/var/lib/postgresql/data",
           "-p", f"127.0.0.1:{PG_PORT}:5432",
           "-e", f"POSTGRES_PASSWORD={PASSWORD}", "-e", f"POSTGRES_DB={DB}",
           POSTGRES_IMAGE, "postgres", "-c", "wal_level=logical")
    containers.append(PG)
    wait("postgres", lambda: subprocess.run(
        ["docker", "exec", PG, "pg_isready", "-h", "127.0.0.1", "-U", "postgres"],
        capture_output=True).returncode == 0)

    psql("create database powersync_storage;", database="postgres")
    psql(BOOTSTRAP)
    print("row model migration", flush=True)
    psql(MIGRATION.read_text())
    psql(REPLICATION)

    print("postgrest", flush=True)
    docker("run", "-d", "--name", REST, "--network", NETWORK,
           "-p", f"127.0.0.1:{REST_PORT}:3000",
           "-e", f"PGRST_DB_URI=postgres://authenticator:{PASSWORD}@{PG}:5432/{DB}",
           "-e", "PGRST_DB_SCHEMAS=public",
           "-e", "PGRST_DB_ANON_ROLE=anon",
           "-e", f"PGRST_JWT_SECRET={SECRET}",
           "-e", "PGRST_SERVER_PORT=3000",
           "-e", "PGRST_LOG_LEVEL=error",
           POSTGREST_IMAGE)
    containers.append(REST)
    wait("postgrest", lambda: rest(mint_jwt(str(uuid.uuid4())), "GET", "/venues?limit=1")[0] < 500)

    print("powersync", flush=True)
    docker("run", "-d", "--name", PS, "--network", NETWORK,
           "-p", f"127.0.0.1:{PS_PORT}:8080",
           "-e", "POWERSYNC_CONFIG_PATH=/config/service.yaml",
           "-v", f"{ROOT / 'service.yaml'}:/config/service.yaml:ro",
           "-v", f"{SYNC_RULES}:/config/sync-rules.yaml:ro",
           POWERSYNC_IMAGE, "start", "-r", "unified")
    containers.append(PS)
    wait("powersync", lambda: subprocess.run(
        ["docker", "exec", PS, "node", "-e",
         "fetch('http://localhost:8080/probes/startup')"
         ".then(r=>process.exit(r.ok?0:1)).catch(()=>process.exit(1))"],
        capture_output=True).returncode == 0, attempts=400)


def account(label):
    """A user that exists in `auth.users`, with a token."""
    user_id = str(uuid.uuid4())
    psql(f"insert into auth.users (id) values ('{user_id}');")
    print(f"  {label} = {user_id}", flush=True)
    return user_id, mint_jwt(user_id)


def check():
    """Owner shares a venue by code; the joiner writes; a stranger sees nothing."""
    failures = []

    def expect(condition, message):
        print(("  PASS " if condition else "  FAIL ") + message, flush=True)
        if not condition:
            failures.append(message)

    print("row-level security", flush=True)
    owner, owner_token = account("owner")
    joiner, joiner_token = account("joiner")
    _, stranger_token = account("stranger")

    venue = str(uuid.uuid4())
    code = "SHARE-" + venue[:8]
    status, body = rest(owner_token, "POST", "/venues",
                        {"id": venue, "name": "Warehouse", "share_code": code},
                        prefer="return=representation,resolution=merge-duplicates")
    expect(status in (200, 201), f"owner inserts a venue ({status} {body})")

    status, body = rest(stranger_token, "GET", f"/venues?id=eq.{venue}")
    expect(body == [], f"stranger cannot see the venue before joining ({body})")

    status, body = rest(joiner_token, "POST", "/rpc/join_venue", {"code": code})
    expect(status == 200 and body == venue, f"joiner joins by share code ({status} {body})")

    status, body = rest(joiner_token, "GET", f"/venues?id=eq.{venue}&select=id")
    expect(body == [{"id": venue}], f"joiner now reads the venue ({body})")

    track, score, clip = str(uuid.uuid4()), str(uuid.uuid4()), str(uuid.uuid4())
    status, body = rest(owner_token, "POST", "/tracks",
                        {"id": track, "track_hash": "h", "title": "Test"},
                        prefer="return=representation")
    expect(status in (200, 201), f"owner inserts a track ({status} {body})")
    status, body = rest(owner_token, "POST", "/scores",
                        {"id": score, "track_id": track, "venue_id": venue, "name": "Set"},
                        prefer="return=representation")
    expect(status in (200, 201), f"owner inserts a score in the venue ({status} {body})")

    status, body = rest(joiner_token, "POST", "/clips", {
        "id": clip, "score_id": score, "graph": "{}", "start": 0.0, "duration": 8.0,
        "seed": "12345678901234567890", "selection_json": "\"All\"",
    }, prefer="return=representation")
    expect(status in (200, 201), f"joiner inserts a clip into the shared score ({status} {body})")

    status, body = rest(owner_token, "GET", f"/clips?id=eq.{clip}&select=id,uid")
    expect(len(body or []) == 1 and body[0]["uid"] == joiner,
           f"owner reads the joiner's clip ({body})")

    status, body = rest(stranger_token, "GET", f"/clips?id=eq.{clip}")
    expect(body == [], f"stranger cannot read the clip ({body})")

    status, body = rest(stranger_token, "GET", f"/tracks?id=eq.{track}")
    expect(body == [], f"stranger cannot read the track ({body})")

    status, body = rest(joiner_token, "GET", f"/tracks?id=eq.{track}&select=id")
    expect(body == [{"id": track}], f"joiner reads the track behind the shared score ({body})")

    status, body = rest(stranger_token, "POST", "/clips", {
        "id": str(uuid.uuid4()), "score_id": score, "graph": "{}", "start": 0.0,
        "duration": 1.0, "seed": "1", "selection_json": "\"All\"",
    })
    expect(status == 403, f"stranger cannot write into the shared score ({status})")

    status, body = rest(stranger_token, "POST", "/rpc/join_venue", {"code": "SHARE-nope"})
    expect(status >= 400, f"a wrong share code does not join ({status})")

    # Verified patterns are the global library.
    pattern = str(uuid.uuid4())
    rest(owner_token, "POST", "/patterns", {"id": pattern, "name": "Strobe", "is_verified": True},
         prefer="return=representation")
    status, body = rest(stranger_token, "GET", f"/patterns?id=eq.{pattern}&select=id")
    expect(body == [{"id": pattern}], f"a verified pattern is readable by everyone ({body})")
    status, body = rest(stranger_token, "PATCH", f"/patterns?id=eq.{pattern}", {"name": "Hijack"})
    expect(status in (200, 204) and rest(owner_token, "GET", f"/patterns?id=eq.{pattern}&select=name")[1]
           == [{"name": "Strobe"}], "a verified pattern is not writable by everyone")

    # One row per venue-child shape, so every sync-rule query in
    # deploy/sync-rules.yaml is evaluated against real data.
    node, child, group, fixture = (str(uuid.uuid4()) for _ in range(4))
    for path, row in [
        ("/venue_nodes", {"id": node, "venue_id": venue, "kind": "venue"}),
        ("/venue_nodes", {"id": child, "venue_id": venue, "kind": "truss"}),
        ("/venue_edges", {"id": child, "child_id": child, "parent_id": node,
                          "my_socket": "base", "their_socket": "top"}),
        ("/venue_node_params", {"id": child + ":length", "node_id": child,
                                "key": "length", "value": 3.0}),
        ("/venue_constraints", {"id": child + ":base", "node_id": child,
                                "my_socket": "base", "target_node": node,
                                "target_socket": "top"}),
        ("/fixtures", {"id": fixture, "venue_id": venue, "address": 1, "num_channels": 8,
                       "manufacturer": "m", "model": "x", "mode_name": "8ch",
                       "fixture_path": "m/x", "address_pinned": False}),
        ("/fixture_groups", {"id": group, "venue_id": venue, "name": "front_wash"}),
        ("/fixture_group_members", {"id": group + ":" + fixture, "group_id": group,
                                    "fixture_id": fixture}),
        ("/cues", {"id": str(uuid.uuid4()), "venue_id": venue, "name": "Blinder",
                   "pattern_id": pattern}),
        ("/midi_modifiers", {"id": str(uuid.uuid4()), "venue_id": venue, "name": "shift",
                             "input_json": "{}"}),
        ("/midi_bindings", {"id": str(uuid.uuid4()), "venue_id": venue,
                            "trigger_json": "{}", "action_json": "{}", "exclusive": False}),
        ("/track_beats", {"id": track, "track_id": track, "beats_json": "[]",
                          "downbeats_json": "[]"}),
        ("/score_definitions", {"id": str(uuid.uuid4()), "score_id": score,
                                "definition_json": "{}"}),
        ("/implementations", {"id": str(uuid.uuid4()), "pattern_id": pattern,
                              "graph_json": "{}"}),
        ("/agent_threads", {"id": str(uuid.uuid4()), "agent_kind": "score"}),
        ("/drafts", {"id": str(uuid.uuid4()), "score_id": score,
                     "base_json": "{}", "state_json": "{}"}),
        ("/changes", {"id": str(uuid.uuid4()), "table_name": "clips", "row_id": clip,
                      "op": "insert", "after_json": "{}"}),
    ]:
        status, body = rest(owner_token, "POST", path, row)
        if status not in (200, 201):
            expect(False, f"insert into {path} ({status} {body})")

    probe = subprocess.run(
        ["docker", "exec", PS, "node", "-e",
         "fetch('http://localhost:8080/probes/startup')"
         ".then(r=>{console.log(r.status);process.exit(r.ok?0:1)}).catch(e=>{console.log(e);process.exit(1)})"],
        capture_output=True, text=True)
    expect(probe.returncode == 0, f"powersync /probes/startup is healthy ({probe.stdout.strip()})")

    # Replication is asynchronous; give it a moment, then insist the service
    # logged no sync-rule evaluation errors for the rows just written.
    time.sleep(5)
    logs = subprocess.run(["docker", "logs", PS], capture_output=True, text=True)
    errors = [line for line in (logs.stdout + logs.stderr).splitlines() if "error" in line.lower()]
    expect(not errors, "powersync replicated every row without a sync-rule error\n"
                       + "\n".join("      " + line for line in errors[:5]))

    # What the joiner's client actually receives over the sync stream.
    delivered = stream(joiner_token)
    shared = {"venues", "venue_members", "fixtures", "fixture_groups", "fixture_group_members",
              "venue_nodes", "venue_edges", "venue_node_params", "venue_constraints",
              "cues", "midi_modifiers", "midi_bindings", "scores", "clips",
              "score_definitions", "tracks", "track_beats", "patterns", "implementations"}
    expect(shared <= delivered.keys(),
           f"the joiner's stream delivers the shared venue ({sorted(shared - delivered.keys())} missing)")
    private = {"agent_threads", "drafts", "changes"}
    expect(not (private & delivered.keys()),
           f"the joiner's stream withholds the owner's private rows ({sorted(private & delivered.keys())})")

    if failures:
        raise RuntimeError(f"{len(failures)} check(s) failed")


def run_tests(names):
    for name in names:
        print(f"cargo test {name}", flush=True)
        result = subprocess.run(
            ["cargo", "+1.97.1", "test", "--manifest-path", str(REPO / "backend/Cargo.toml"),
             "-p", "luma", "--lib", name, "--", "--ignored", "--nocapture"],
            cwd=REPO, env={**os.environ, **ENVIRONMENT})
        if result.returncode:
            raise RuntimeError(f"test failed: {name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", action="store_true", help="leave the containers running")
    parser.add_argument("--sql", action="store_true", help="open psql against the stack")
    parser.add_argument("--no-check", action="store_true", help="skip the row-level security checks")
    parser.add_argument("--test", action="append", default=[], metavar="NAME",
                        help="a Rust test to run against the stack; repeatable")
    args = parser.parse_args()

    refuse_existing()
    containers = []
    failed = False
    try:
        start(containers)
        for key, value in ENVIRONMENT.items():
            print(f"{key}={value}", flush=True)
        if not args.no_check:
            check()
        if args.test:
            run_tests(args.test)
        if args.sql:
            subprocess.run(["docker", "exec", "-it", PG, "psql", "-U", "postgres", "-d", DB])
    except Exception as error:
        failed = True
        print(f"FAILED: {error}", file=sys.stderr, flush=True)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stderr or error.stdout, file=sys.stderr, flush=True)
        for name in containers:
            logs = subprocess.run(["docker", "logs", "--tail", "40", name], capture_output=True, text=True)
            print(f"--- {name}\n{logs.stdout}{logs.stderr}", file=sys.stderr, flush=True)
    finally:
        if args.keep:
            print(f"kept: {' '.join(containers)} (docker rm -f them when done)", flush=True)
        else:
            for name in reversed(containers):
                subprocess.run(["docker", "rm", "-f", name], check=False, capture_output=True)
            subprocess.run(["docker", "network", "rm", NETWORK], check=False, capture_output=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())

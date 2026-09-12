#!/usr/bin/env python3
"""Disposable Postgres, PostgREST and PowerSync for the row model.

Starts the three services a Luma client talks to, applies the row model to an
empty database, and runs whichever Rust tests were named against the live
stack. Older migrations are not applied: the row-model migration is
self-sufficient by design.
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
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
MIGRATIONS = [
    REPO / "supabase/migrations/20260912000000_row_model.sql",
    REPO / "supabase/migrations/20260916000000_stage_child_venue_id.sql",
]
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

USAGE = f"""\
containers, on loopback only, removed on exit unless --keep:

  {PG:<22} {POSTGRES_IMAGE:<38} 127.0.0.1:{PG_PORT}   wal_level=logical
  {REST:<22} {POSTGREST_IMAGE:<38} 127.0.0.1:{REST_PORT}
  {PS:<22} {POWERSYNC_IMAGE:<38} 127.0.0.1:{PS_PORT}

examples:

  run.py --test sync::two_device_tests    run the Rust suite against it
  run.py --keep --sql                     leave the stack up, open psql on it

--test runs, once per NAME, from the repo root:

  cargo +1.97.1 test --manifest-path backend/Cargo.toml -p luma --lib NAME \\
      -- --ignored --nocapture

with the stack in the environment, which is how the Rust tests find it:

  LUMA_TEST_POWERSYNC_URL={PS_URL}
  LUMA_TEST_POSTGREST_URL={REST_URL}
  LUMA_TEST_PG_URI={PG_URI}
  LUMA_TEST_JWT_SECRET={SECRET}

That secret is also the inline JWKS key in this directory's service.yaml, so
one minted token is accepted by both PostgREST and PowerSync. PowerSync reads
that service.yaml and the repo's deploy/sync-rules.yaml, so the sync rules
under test are the ones that ship.

The runner refuses to start if a {PREFIX}* container already exists, and never
removes one it did not create; after --keep, `docker rm -f` them yourself.
"""

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

# The accounts `backend/src/sync/two_device_tests.rs` signs its devices in as.
# Fixed, because the server's foreign keys are real: a row whose `uid` is not in
# `auth.users` is refused, and a test that minted its own would have to tell the
# Rust side what it minted.
TEST_USERS = [
    "00000000-0000-0000-0000-0000000000aa",
    "00000000-0000-0000-0000-0000000000bb",
    "00000000-0000-0000-0000-0000000000cc",
]


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
    for migration in MIGRATIONS:
        print(f"migration {migration.name}", flush=True)
        psql(migration.read_text())
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
    wait("postgrest", lambda: urllib.request.urlopen(
        urllib.request.Request(f"{REST_URL}/venues?limit=1",
                               headers={"Authorization": f"Bearer {mint_jwt(TEST_USERS[0])}"}),
        timeout=5).status < 500)

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


def run_tests(names):
    for user in TEST_USERS:
        psql(f"insert into auth.users (id) values ('{user}') on conflict do nothing;")
    for name in names:
        print(f"cargo test {name}", flush=True)
        result = subprocess.run(
            ["cargo", "+1.97.1", "test", "--manifest-path", str(REPO / "backend/Cargo.toml"),
             "-p", "luma", "--lib", name, "--", "--ignored", "--nocapture"],
            cwd=REPO, env={**os.environ, **ENVIRONMENT})
        if result.returncode:
            raise RuntimeError(f"test failed: {name}")


def main():
    parser = argparse.ArgumentParser(
        description="Bring up a disposable Postgres + PostgREST + PowerSync stack for the "
                    "row model, and run Rust tests against it.",
        epilog=USAGE,
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--keep", action="store_true",
                        help=f"leave the {PREFIX}* containers running after the run")
    parser.add_argument("--sql", action="store_true",
                        help=f"open an interactive psql on {PG} (use with --keep to stay)")
    parser.add_argument("--test", action="append", default=[], metavar="NAME",
                        help="a Rust test filter to run against the stack; repeatable")
    args = parser.parse_args()

    refuse_existing()
    containers = []
    failed = False
    try:
        start(containers)
        for key, value in ENVIRONMENT.items():
            print(f"{key}={value}", flush=True)
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

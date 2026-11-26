# Database manager: design proposal

Status: stage 1 is implemented on the `feature/database-manager` branch but has not been compiled, run or tested
yet (it was written without a Rust toolchain at hand); stages 2 to 4 are a proposal. This file turns into a guide
once the code is tested. What stage 1 does, where it differs from this proposal and how to try it:
[Stage 1 as implemented](#stage-1-as-implemented).

## Goal

A web page to manage the databases of one host, in the spirit of Odoo's database manager and selector:

- list the databases with their size and state,
- create a database (name, administrator login and password, demo data, modules),
- duplicate a database,
- back up a database to a downloadable file, and restore one from a file,
- delete a database,
- choose which database to open.

## What the framework does today

These constraints come from the current code and guides, and the design keeps all of them:

- **One server process serves one database.** `server.toml` names a single `database`, and
  `erp-server` refuses a request whose `X-ERP-Database` header or cookie names another one
  (`crates/erp-server/src/boundary.rs`, `database`).
- **Four roles per database**, kept apart on purpose: owner (schema and module operations), request (all
  ordinary work; not a superuser, cannot create roles or databases), node (registration and heartbeat) and a
  `NOLOGIN` maintenance role ([install](../install.md#create-the-roles-and-the-database)).
- **Module operations run while the server is stopped**, with the owner URL in `DATABASE_URL`.
- **Secrets live in mode 600 files**, never on a command line or in a log.
- `scripts/install.sh` already performs the whole creation sequence for one local database: roles and
  database, `erp db init`, `erp module install`, `erp db publish-grants`,
  `erp cluster login-selection publish`, server start, readiness check.

A manager inside the request server would need a role that can create and drop databases, which is exactly
what the request role is denied. Serving several databases from one server process would change the
registry, assets and node model at their core. Neither is proposed.

## Proposal: `erp manager`, a separate process

A new `erp manager` subcommand in `erp-cli` serves the manager pages. It is a different process from
`erp server`, with its own configuration and its own credentials, and it is off unless an operator starts it.

### Configuration (`manager.toml`, mode 600)

| Key | Meaning |
|---|---|
| `bind` | Address of the manager pages. Loopback by default; any other address is refused unless `allow_remote = true` and `public_origin` is HTTPS. |
| `master_password_file` | PHC hash of the manager password. |
| `cluster_admin_url_file` | URL of a PostgreSQL role with `CREATEDB` and `CREATEROLE` (or a superuser), used only by the manager. |
| `instances_dir` | Where each database gets its own directory: URL files, `server.toml`, log and pid. |
| `erp_binary`, `addons_path`, `assets_manifest` | What a new instance runs with, as in `server.toml`. |
| `port_range` | Ports handed to instances, one per database, on loopback. |
| `supervise` | `true`: the manager starts and stops the `erp server` process of each instance (local machines). `false`: it only manages databases and leaves processes to the host's supervisor, such as systemd. |

### Instances

One database is one **instance**: the database, its four roles named after it (as `install.sh` names them),
and a directory under `instances_dir` with the mode 600 URL files and a `server.toml` that names only that
database. The request server keeps its least-privilege roles; the manager's cluster admin role never reaches
it.

### Pages

- `/manager/login`: the master password, with the same CSRF and same-origin checks as `/erp/login`, and a
  rate limit.
- `/manager/`: the list of instances (name, size, state, port, link) with their actions.
- Create, duplicate, back up, restore and delete forms. Delete asks for the database name to be typed.
- `/manager/password`: change the master password.
- `/` (selector): the running instances, each linking to its own `/erp/login`.

### Operations

| Operation | Steps |
|---|---|
| Create | Roles and database as the cluster admin; write the URL files and `server.toml`; `db init`; `module install`; `publish-grants`; `login-selection publish`; `publish-grants`; start and wait for `/erp/health/ready`. The same order as `install.sh`. |
| Back up | Stop nothing; `pg_dump --format=custom` with a PostgreSQL 18 client, plus the attachment store when it is outside the database; one archive with a manifest of the source name, versions and installed modules. |
| Restore | New roles and database; `pg_restore --no-owner --role=<new owner>`; hand ownership to the new roles; `publish-grants` and `login-selection publish` for them; restore the attachment store; start. |
| Duplicate | Back up and restore into a new name in one step, so that a running source needs no downtime. |
| Delete | Stop the instance, end its connections, drop the database and its four roles, remove its directory. |

Every operation writes an audit line (who, what, when, result) to a log next to `manager.toml`, and fails
loudly with the step that refused.

## To verify before the code

1. Whether role names are stored inside a database (published SQL objects, login-selection helpers,
   policies, grants), so that restore and duplicate must rewrite them for the new roles, or whether
   `publish-grants` and `login-selection publish` rebuild all of it.
2. Where attachment content lives for each `attachments` store setting, and how the store is copied.
3. How `erp db init` receives the administrator password when the manager collects it in a form (a
   temporary mode 600 file, as today, or stdin).
4. A single public origin behind a reverse proxy: the selector needs one hostname or path prefix per
   instance, because each instance is its own server. Local use with one port per instance needs none.

## Stages

1. Read-only list, selector, and start and stop of instances. Implemented, not yet compiled (below).
2. Create and delete.
3. Back up and restore.
4. Duplicate.

Each stage comes with integration tests through `erp-testkit` against PostgreSQL 18, as the repository's
rules ask.

## Out of scope

Serving several databases from one `erp server` process, and any change to the request server's roles.

## Stage 1 as implemented

`erp manager serve --config manager.toml` serves the stage 1 pages; the code is in `crates/erp-cli/src/manager.rs`
and `crates/erp-cli/src/manager/`. It opens no database and never reads an instance's credential files.

- **Configuration.** `manager.toml` and the master password file are read with the server's own secret-file reader
  (`erp_server::config::read_secret`): regular files of mode 600, not symbolic links, at most 4096 bytes. The keys
  are `bind` (default `127.0.0.1:8070`), `allow_remote`, `public_origin`, `master_password_file`, `instances_dir`
  and `erp_binary` (default: the running `erp`). A `bind` that is not loopback is refused unless
  `allow_remote = true` and `public_origin` is HTTPS. An unknown key is refused. Relative paths are taken from the
  directory of `manager.toml`.
- **Master password.** A PHC hash, checked with `erp_security::auth::password::StoredHash` (Argon2id or
  PBKDF2-SHA512, as for user passwords). `erp manager hash-password --password-file F --output H` writes one with
  the default parameters to a new mode 600 file.
- **Instances.** A subdirectory of `instances_dir` that holds `private/server.toml`: the layout `scripts/install.sh`
  writes. The manager reads `database`, `bind` and `public_origin` from that file and nothing else.
- **State.** Running: the pid in `server.pid` is alive, not a zombie, and its command name contains `erp` (the rule
  `install.sh` applies with `ps`), and `GET /erp/health/ready` answers 200. Starting: alive but not ready. Stopped:
  anything else.
- **Start and stop.** Start runs `erp server --config <instance>/private/server.toml` in a process group of its
  own, appends its output to `server.log` and writes `server.pid`, as `install.sh` does; it is refused while the
  instance runs or while something else holds its port. Stop sends SIGTERM with `kill` and waits up to 60 s.
- **Pages.** `/` lists the running instances, each linking to its own `/erp/login`, without sign-in.
  `/manager/login` asks for the master password; `/manager/` lists every instance with its database, port and
  state, and Start, Stop and Open. Every POST passes the same-origin rule of `/erp/login` and carries a CSRF token:
  on the sign-in form a token that must equal a cookie only that form receives, afterwards the session's token.
  Sessions live in memory for 8 hours. Cookies are `HttpOnly`, `SameSite=Strict`, limited to the path `/manager`
  (a browser sends the cookies of `127.0.0.1` to every port, so the instances would see them otherwise), and
  `Secure` behind an HTTPS origin. Five failed sign-ins from one address within 5 minutes make it wait (behind a
  reverse proxy every client has the proxy's address, so the limit is shared).
- **HTTP.** A small HTTP/1.1 reader of its own: one request per connection, a bounded head and body, no chunked
  bodies, one thread per connection and at most 16 at once. `erp-cli` does not depend on `hyper` directly, and the
  manager stays out of the request server.

Unit tests sit next to the code: configuration and its refusals, the permission checks, instance discovery from a
temporary directory, the state rule, the pid file, the readiness probe against a local listener, the HTTP reader,
form decoding, CSRF tokens, sessions, the rate limit and cookies.

### Gaps against this proposal

- No database size on the list: it needs a database connection, which stage 1 does not open.
- No `/manager/password` page: write a new hash with `erp manager hash-password` and restart the manager.
- No audit log of operations yet.
- No integration test through `erp-testkit` yet; stage 1 has unit tests only.
- `cluster_admin_url_file`, `addons_path`, `assets_manifest`, `port_range` and `supervise` belong to later stages.
- The manager does not start PostgreSQL. An instance installed with the Docker option needs its container running
  (`docker start <container>`, or run `scripts/install.sh` again for that state directory).
- Sessions are in memory: restarting the manager signs everyone out.

### Try it

A local install made with `scripts/install.sh` lives in `~/.erpforall/local` and is already an instance. Each
further instance needs its own state directory, database, port and, with the Docker option, container:

```sh
scripts/install.sh --state-dir ~/.erpforall/shop --db-name shop --port 8071 --pg-container shop-postgres
```

Then give the manager a directory of its own next to them, with a password hash and a configuration whose
`instances_dir` is `~/.erpforall` (the manager's directory holds no `private/server.toml`, so it is not listed):

```sh
mkdir -p ~/.erpforall/manager && cd ~/.erpforall/manager
(umask 077; cat > password)        # type the master password, Enter, then Ctrl-D
erp manager hash-password --password-file password --output master.phc
rm password
cat > manager.toml <<'TOML'
master_password_file = 'master.phc'
instances_dir = '..'
TOML
chmod 600 manager.toml
erp manager serve --config manager.toml
```

`erp` is the binary of the checkout (`target/release/erp` after `scripts/install.sh`). Open
`http://127.0.0.1:8070/manager/` and sign in with the master password; `http://127.0.0.1:8070/` is the selector.

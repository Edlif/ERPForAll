# Database manager: design proposal

Status: proposal, not implemented. This file is the starting point of the `feature/database-manager` branch and
turns into a guide once the code exists.

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

1. Read-only list, selector, and start and stop of instances.
2. Create and delete.
3. Back up and restore.
4. Duplicate.

Each stage comes with integration tests through `erp-testkit` against PostgreSQL 18, as the repository's
rules ask.

## Out of scope

Serving several databases from one `erp server` process, and any change to the request server's roles.

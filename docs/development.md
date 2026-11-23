# Developing ERPForAll

This guide is a map for people changing ERPForAll or writing addons for it. Installing and running a server is in
the [installation guide](install.md); running one for real use is in the [deployment guide](deployment.md). The
rules for contributions (licence, sign-off, pull requests) and the build and test commands are in
[CONTRIBUTING.md](../CONTRIBUTING.md); they are not repeated here.

## Repository layout

| Path | What is in it |
|---|---|
| `crates/` | The Rust framework, one crate per concern (below). |
| `addons/` | Addons that ship in this repository: `base`, `web`, `uom`, `product`, `contacts`, `mail`, the stock family, a theme, and `sample_library` (a data-only example). |
| `web/` | The web client runtime ("Elang") as a pnpm workspace: `src/` (runtime), `packages/`, `tests/`, `scripts/`, `vendor/` (vendored web libraries). The client side of each addon lives next to the addon, in `addons/<name>/static/`. |
| `examples/` | Sample addons built as WebAssembly components (`examples/wasm`). |
| `wit/` | The WebAssembly interface (WIT) that addon components implement, with its golden files. |
| `fuzz/` | Fuzz targets. |
| `vendor/` | Patched copies of third-party crates, wired in through `[patch.crates-io]` in the root `Cargo.toml`. They are not workspace members and keep their own licences. |
| `testdata/` | Shared fixtures used by tests. |
| `scripts/` | Helper scripts: `check.sh` (format, lints, build, tests), `web-toolchain.sh` (pinned Node and pnpm), generators and checks. |
| `docs/` | These guides. |

### Crates

The framework crates are layered: a crate depends only on crates below it. `scripts/check-layers.py` enforces the
rule, and addon crates may depend only on `erp-sdk`.

| Crate | Role |
|---|---|
| `erp-fields`, `erp-domain`, `erp-expr`, `erp-html`, `erp-sql`, `erp-store-wire` | Pure building blocks: field types and values, the search-domain language, the expression evaluator, the HTML sanitiser, SQL text builders, the messaging wire format. |
| `erp-registry`, `erp-views` | Model metadata and inheritance; view architecture inheritance. |
| `erp-orm` | The ORM: environments, transactions, recordsets, schema sync. |
| `erp-security` | Access rules, groups, authentication and sessions. |
| `erp-data`, `erp-modules` | Data files (XML, CSV, external ids) and the module system: manifests, dependency graph, install, upgrade, uninstall. |
| `erp-rpc`, `erp-server` | The JSON-2 call layer and the HTTP server. |
| `erp-sdk`, `erp-macros`, `erp-guest`, `erp-wasm-host` | The addon API and its macros; the WebAssembly guest bindings and host. |
| `erp-ui-compiler`, `erp-ui-compiler-cli` | The template compiler for the web client and its `erp-ui-compile` program. |
| `erp-cli` | The `erp` program. |
| `erp-suite-*` | Isolated L13 domain integration test suites (`crates/erp-suite-<domain>`): depend only on core crates and their domain addon closure, never on `erp-cli`. |
| `erp-testkit`, `erp-test-models`, `erp-alloc-budget` | Test support: throwaway databases, core fixture addons, allocation budgets. |

## Building and testing

Use the commands in [CONTRIBUTING.md](../CONTRIBUTING.md#build-and-test). `scripts/check.sh --affected --tests` narrows
clippy and tests to the Cargo packages affected by your diff against `origin/main` (using `scripts/affected-packages.py`
and `scripts/check-layers.py`); `-p <pkg>` targets explicit packages, and `scripts/check.sh --tests` runs the full
workspace. Tests that need a database create and drop their own databases on the PostgreSQL server named by the
`DATABASE_URL` environment variable (or, for several servers, a space-separated `ERP_TEST_DATABASE_URLS`); the role must
be allowed to create databases.

## How a change flows

1. Write the behaviour down as a test first, next to the code it covers (or in a domain suite crate `crates/erp-suite-<domain>` for multi-addon integration tests). One test per behaviour; name it after
   what it proves.
2. Make it pass. **Fail loudly:** return a clear error for anything unsupported; never substitute a default or
   swallow an error to look finished.
3. Keep the core small. Business domains are addons, not core.
4. Run `scripts/check.sh --affected --tests`, and for web changes the web checks below.
5. Open a pull request: one topic, tests included, commits signed off.

## Writing an addon

An addon is a directory with a `manifest.toml`. Scaffold one:

```sh
erp scaffold my_addon --summary "My custom module" --author "Your Name"      # native Rust addon, in ./addons/my_addon
erp scaffold my_tags --data-only --summary "Declarative tags"               # data and views only
```

A native addon is a Rust crate with these parts (this is what `erp scaffold` writes):

```text
my_addon/
├── Cargo.toml          # depends on erp-sdk
├── manifest.toml       # name, summary, version, licence, author, depends, data files
├── src/lib.rs          # erp_sdk::addon!(name = "my_addon", depends = ["base"]);
├── src/models.rs       # models declared with #[model]
├── views/              # form and list views in XML
├── data/               # records and seed data in XML or CSV
└── tests/              # integration tests
```

Models are Rust structs:

```rust
use erp_sdk::prelude::*;

#[model(name = "my_addon.record", description = "My custom module")]
pub struct Record {
    #[field(char, default = "")]
    pub name: Option<Arc<str>>,
}
```

A data-only addon has no Rust code: models go in `models/*.toml`, and views, menus, access rows and demo data in data
files. `addons/sample_library` is a worked example, with `manifest.toml` keys such as `depends`, `data` (views,
`security/ir.access.csv`) and `demo`. The pieces an addon can contribute are models, extensions of models from other
addons, views and view patches (XPath), menus and actions, access rules and record rules, data and demo data, and
client-side code for the web client.

To add a native addon to this repository, put the crate under `addons/`, list it in the root `Cargo.toml`
(`[workspace] members` and `[workspace.dependencies]`, like the existing `addons/*` entries) and rebuild `erp`.
`scripts/check-layers.py` checks that the addon only depends on allowed crates.

To try an addon: install it into a database as shown in the [installation guide](install.md#install-an-addon-from-a-folder).

An addon can rebrand the server by shipping `static/brand/icon.svg`, `logo.svg` and `logo-dark.svg` and listing
them under `static_files` in its asset manifest (`static/dist/manifest.json`). Once it is installed, `/favicon.ico`
and `/erp/brand/icon.svg`, `logo.svg` and `logo-dark.svg` (used by the login page, the error page and the
application) answer with its images; an image it does not ship keeps the built-in one. The responses carry an
`ETag` and `Cache-Control: no-cache`, and SVG is sent with a sandboxing `Content-Security-Policy`. Only one
installed addon may ship brand images: installing a second one fails.

A form view opens a saved record editable. `<form edit_mode="read">` opens it read-first instead: the fields show as
read values with an Edit button (or the E key outside a field); a successful Save or a Discard return to read mode,
a refused save stays in edit mode with its errors, and a new record opens editable. A form with `edit="0"` is always
read-only and cannot declare read mode. A field widget of the record form receives `mode` (`read` or `edit`) and
`onRecordEvent(listener)`, which returns an unsubscribe function and reports `saved` (with the record id),
`saveFailed`, `discarded` and `modeChanged` after the form has shown the outcome; subscriptions end with the widget.
### The request's client address

A method called over JSON-2 can read where the request came from: `env.request()` is the client address the server
resolved, the peer or, behind a [trusted proxy](deployment.md#https-behind-a-reverse-proxy), the address the proxy
forwarded. It is `None` for jobs, the command line, tests and internal calls, so a rule must decide what "unknown"
means. A client cannot set it: it is not part of the context, and a forwarded address from a peer that is not a
trusted proxy is refused before any method runs. `erp_sdk::net::IpNetwork` parses CIDR ranges and matches IPv4
clients seen through an IPv6 socket (`::ffff:a.b.c.d`).

```rust
use erp_sdk::net::IpNetwork;

let office: Vec<IpNetwork> = ["10.20.0.0/16", "2001:db8:20::/48"]
    .iter()
    .map(|text| text.parse())
    .collect::<Result<_>>()?;
match tx.env().request().map(|request| request.client_address()) {
    Some(address) if office.iter().any(|network| network.contains(address)) => {}
    Some(_) => return Err(Error::user("Clock-in is allowed only from the office network.")),
    None => return Err(Error::user("The network of this request is unknown.")),
}
```

### Recurring jobs of an addon

A native addon runs work on a schedule by giving one of its step job kinds a `Schedule`:

```rust
use erp_sdk::jobs::{HandlerRef, JobKindDecl, Schedule};

erp_sdk::jobs::register_job_kind!(
    DAILY_REFRESH,
    JobKindDecl::new(
        "my_addon",
        "my_addon.daily_refresh",
        HandlerRef::Step { model: "my_addon.record", method: "refresh_step" },
    )
    .schedule(Schedule::daily_at(1, 0, "Asia/Jakarta", "my_addon.user_scheduler"))
);
```

- The recurrence is `Schedule::daily_at(hour, minute, time_zone, run_as)` (wall-clock time in an IANA zone) or
  `Schedule::every(interval, run_as)` / `Schedule::every_from(interval, offset, run_as)` (every 60 s to 31 days,
  aligned to the Unix epoch plus the offset).
- `run_as` is the external id of an active internal `res.users` record the addon ships in its data, a technical
  user whose access rights the job runs with. A scheduled kind never runs as the system.
- Installing or upgrading the addon creates the kind's one job row, due at the next instant; uninstalling it removes
  the row (refused while it runs). A row that already exists keeps its due time, and a row an operator held stays held.
  The row cannot be enqueued by code.
- The row runs on whichever node runs jobs (a server with `jobs_run_in_app = true`); with several nodes it still runs once per
  due time. The step receives the payload `{"scheduled_for": "<RFC 3339>", "previous": "<RFC 3339>" | null}`:
  the instant this run stands for and the one the last completed run stood for. Runs missed while no node ran jobs
  collapse into one run for the latest missed instant, so a step that processes "the period since `previous`" stays
  correct and idempotent.
- When the step reports `finished`, the row is rescheduled to the next instant instead of being removed, and a kind
  that keeps history writes one `done` row per run. A failing run is retried with the kind's backoff and, when its
  attempts run out, dead-lettered like any job; the schedule then waits for an operator to requeue it, and resumes the
  same run.
The same addon can name the product and theme the sign-in pages:

- `static/brand/brand.json` is exactly `{"name": "<product name>"}`: plain text, 1 to 64 characters, no control
  or invisible formatting characters, no leading or trailing whitespace. The name replaces "ERPForAll" in the
  `<title>` of the login, login-success, error and application pages and in the logo text of the login page,
  the error page and the application header (the server sends it as `<meta name="application-name">`, always
  HTML-escaped). A manifest that is anything else fails the install, and the server refuses it again when it
  publishes the assets.
- `static/brand/login.css` is linked after the client stylesheet on the login and login-success pages, as
  `/erp/brand/login.css` (same headers as the brand images, served from the published assets; not found when no
  addon ships one). It is a theme, not new markup: set the `--login-*` custom properties
  (`--login-bg`, `--login-card`, `--login-fg`, `--login-line`, `--login-input-*`, `--login-link`, ...) and the
  colour tokens (`--accent`, `--canvas`, `--surface`, ...) on `:root` or `.login-screen`, and style the
  `.login-screen`, `.login-brand`, `.login-logo`, `.login-card`, `.login-field`, `.login-submit`,
  `.login-error`, `.login-hint` and `.login-footer` classes. Other selectors are not a supported surface and may
  change. The page policy is unchanged: the stylesheet is same-origin (`style-src-elem 'self'`), and the images
  and fonts it uses must be same-origin assets of the addon. The error page keeps its own built-in style.

Both files count as brand files: they must come from the addon that ships the brand images, if any.

### Retried writes: `Idempotency-Key`

A client that may send the same change twice (a phone with an offline outbox, a script that retries on a timeout)
sends an `Idempotency-Key` header with a JSON-2 call that changes data:

```text
POST /json/2/my_addon.record/submit
Authorization: Bearer <api key>
Idempotency-Key: 6f1c0a7e-1b8e-4a33-9a1f-2a6c4d1e9b10
```

- The key is a UUID (any version; v7 is recommended), one per user action, reused unchanged on every retry of
  that action. It is claimed in the same transaction as the change, so the change and its key commit or roll back
  together; a failed call stores nothing and may be sent again.
- Keys are private to the caller: the user, and for a Bearer request also the API key. The same key from another
  user or another API key is a different key.
- A retry with the same body within one hour gets the first answer again, byte for byte, with the response header
  `Idempotent-Replayed: true`; the method does not run again. Key order and whitespace of the JSON body do not
  matter.
- A key stays claimed for 24 hours. A retry after the first hour, or of an answer larger than 1 MiB, gets 409
  `erp.idempotency_result_expired` ("the original call committed; its result is no longer available"): the
  change happened, read it back instead. After 24 hours the key is free again and the request executes as new.
- The same key with a different body is refused with 422 `erp.idempotency_mismatch`; a retry while the first
  request still runs gets 409 `erp.idempotency_in_progress` with `Retry-After`; a key on a call whose effect is
  `read` is refused with 422 `erp.idempotency_not_allowed`. A malformed key, or the header sent twice, is 400.

A method reads the key it runs under with `tx.idempotency_key()` (`None` without the header, and in every
transaction the RPC layer did not open: jobs, cron runs and scripts never inherit it). Every retried attempt of
the same call sees the same key. Storing it on the record the method creates lets the client show which
submission a record came from, and lets the method answer the existing record when a retry arrives after the
24 hours instead of creating a second one:

```rust
#[method]
#[rpc(effect = write, model)]
async fn submit(
    this: &RecordSet<Request>,
    tx: &mut Transaction,
    note: String,
) -> Result<RecordSet<Request>> {
    let key = tx.idempotency_key().map(str::to_owned);
    if let Some(key) = &key {
        let existing = this
            .search(tx, &Domain::field("submission_key")?.eq(key.as_str()))
            .await?;
        if !existing.as_records().is_empty() {
            return Ok(existing);
        }
    }
    let mut vals = this.vals().set(Request::NOTE, note.as_str());
    if let Some(key) = &key {
        vals = vals.set(Request::SUBMISSION_KEY, key.as_str());
    }
    this.create(tx, vec![vals]).await
}
```

## The web client

The client is TypeScript, compiled ahead of time:

- `web/src` is the runtime (components and reactivity); `web/packages` holds shared packages such as the
  expression evaluator.
- The views, widgets and shell of the application are in the `web` addon: `addons/web/static/`.
- XML templates are compiled by `erp-ui-compile` and bundled by `web/scripts/build-human-ui.mjs`, as shown in the
  installation guide.
- A binary field shows as a file (the `binary` widget, also used when the view names no widget): its name, its size
  and Download, plus Upload or Replace and Clear when editable. `filename="document_name"` (or the `filename` option)
  names the char field of the same record that holds the file's name; Upload writes both. The
  `accepted_file_extensions` option (`'.pdf,image/*'`) is a hint for the file chooser; the server stays the
  authority. Use `widget="image"` for pictures.

A menu's icon (`web_icon` on `<menuitem>` or `ir.ui.menu`) is one of:

- `lucide:<name>`, a glyph the client ships (the menu icon picker lists them);
- `<addon>,static/<path>.svg` or `.png`, an image of the addon: list `static/<path>` under `static_files` in the
  addon's asset manifest (`static/dist/manifest.json`) so the server publishes it at `/<addon>/static/<path>`.
  The client draws it as `<img>` (an SVG there runs no script, and the server sends SVG files with a sandboxing
  `Content-Security-Policy`); a file that is not published shows the default glyph;
- a `data:image/svg+xml` or `data:image/png` URL of at most 2048 bytes.

The same icon is drawn on the home screen, in the navigation and in Settings. Any other value fails the write that
stores it, so an addon with a mistyped icon fails to install.

When the server refuses the window action an address names (403, for example a bookmark to an app the user may no
longer open), the shell keeps its header and menus and shows "This screen is not open to your account." with a "Go to
home" button; the address becomes the home address with the same companies, so Back does not return to the refusal.
Nothing the server said about the action is shown. Other failures still open the error dialog. An addon may replace
the body of that page (at most one page is registered) with `registerRefusedActionPage(key, { create(env) })` from
`public.ts`: `env` has `rpc` and `navigate(route)`, never the refused action; `create` returns
`{ element, dispose }`, and a `create` that throws is reported and the fixed page is shown instead.

### Active companies

The companies a page works in are the `allowed_company_ids` of every call, current company first. The shell
bootstrap (`GET /erp`) decides them once per load:

1. `cids` in the address (`/erp/action-42?cids=3,1`) wins for that load. A link carries its companies, and opening
   it does not change anything stored.
2. Otherwise, the companies the user last confirmed in the company switcher. The switcher stores them with
   `res.users.settings` / `set_res_users_settings({"active_company_ids": [3, 1]})` before it reloads; `False`
   clears the choice. The list must be non-empty, distinct, at most 64 ids, and only of the user's allowed companies:
   anything else is refused, on the generic `write` path too.
3. Otherwise, the user's default companies (main company first).

A stored choice never widens access. `get_res_users_settings` and the bootstrap keep only the companies the user is
still allowed, in the stored order. When none is left, the default companies apply again.

### Navigation from addon code

Systray items, service controllers, client actions and field widgets (in the main form, list cells and kanban cards;
not in dialogs or one2many lines) receive
two functions from the shell (`ShellNavigation` in `addons/web/static/src/client/registry.ts`, exported from
`public.ts` with the `Route` and `ActionRequest` types):

- `navigate(route)` opens a route through the shell's guards (an unsaved record is saved or the user is asked). The
  page's companies (`cids`) stay unless the route names its own; other companies load the page again with them, as
  the company switcher does. A malformed route or one that names a `method_action` is refused.
- `openAction({xmlId, viewMode, domain, context})` opens a window action by XML id as its list (or a new record in
  its form) with the given domain and `default_<field>` context keys, with a breadcrumb back to the page it was
  called from. Other context keys are refused.

Both do nothing once the page that handed them out was left. The address of every page keeps the active companies,
so a reload or a copied link opens the same companies.

A user's home action (`res.users.action_id`, a window action the user may set on their own record) opens in place of
the app grid when the address names no action. It loads through the usual access checks: an action the user may not
open leaves the grid with a notice. The logo still opens the grid.

Checks (after `eval "$(scripts/web-toolchain.sh env)"` and `pnpm --dir web install --frozen-lockfile`):

```sh
cd web
pnpm run typecheck
pnpm run test
```

An object button in a form can ask for one short answer before its method runs, instead of a one-field wizard:

```xml
<button name="action_refuse" type="object" string="Refuse"
        prompt="text" prompt-label="Reason" prompt-required="1" prompt-max="500"/>
<button name="action_run" type="object" string="Run" prompt="date" prompt-label="Run for date"/>
<button name="action_close" type="object" string="Close"
        prompt="selection" prompt-label="Outcome" prompt-options="won:Won,lost:Lost"/>
```

The answer arrives as the `prompt_value` keyword argument: text, an ISO date or the chosen value, and `null` for an
empty optional answer. The method must take `prompt_value: String` (or `Option<String>` when the prompt is not
required); the view is refused otherwise. The client checks required, length, date and choice before calling, but the
method can be called directly over RPC, so it validates its argument like any other. A prompt cannot combine with
`confirm`, and a wizard stays the way for several fields.
## Printed reports for native clients

`GET /erp/report/pdf/<report>/<ids>` (`?download=1` for an attachment) answers the PDF of a report. A browser sends
its session cookie; a native client without one (a mobile app) sends the API key it signed in with instead:

```http
GET /erp/report/pdf/my_addon.report_document/42?download=1
Authorization: Bearer <key>
```

The key is checked as on JSON-2 (an `rpc` user key, the address throttle, a foreign `Origin` refused, a cookie sent
alongside must belong to the key's user) and its user is the one the report prints for: the report's groups, read
access on every id and the id limits apply as for that user's browser session. The answer is
`application/pdf` with `Cache-Control: private, no-store` and no cookie. A refusal is the JSON error of JSON-2 (or a short text), not the HTML error page a browser
navigation gets. The HTML preview
(`/erp/report/html/...`) stays with the browser session; a key is refused there.

## Pull request rules

- One topic per pull request, with tests for the behaviour you change.
- Errors fail loudly; no silent defaults.
- No code copied from other ERP projects or from any source whose licence does not allow it.
- Every commit carries a sign-off (`git commit -s`).
- This project is built with AI agents alongside people; agents follow the same rules, and the person who submits
  the change signs it off.

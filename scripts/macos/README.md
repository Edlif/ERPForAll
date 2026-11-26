# ERPForAll.app for macOS

A small app that starts and stops your local ERPForAll server from the Dock, so you do not need a terminal for
daily use. It is a thin window over `erpforall-ctl`, a shell script that drives the installation
`scripts/install.sh` makes. The app adds no state of its own: the database, the configuration and the admin
password stay where `install.sh` keeps them, in `~/.erpforall/local`.

## What you need

The same as `scripts/install.sh`: a Mac with Apple Silicon, the Xcode command line tools
(`xcode-select --install`), Rust through [rustup](https://rustup.rs), and Docker Desktop or OrbStack for the
PostgreSQL 18 container. The checkout path must not contain spaces.

## Build the app

From the checkout, on the Mac:

```sh
git clone https://github.com/IndoERP/ERPForAll.git ~/src/ERPForAll
cd ~/src/ERPForAll
scripts/macos/build-mac-app.sh            # writes ~/Applications/ERPForAll.app
```

The app calls `erpforall-ctl` in this checkout by its absolute path. If you move the checkout, run
`build-mac-app.sh` again.

## Use it

Open **ERPForAll** from Applications in your home folder. The window shows the current state and the next
step:

| State | What the app offers |
|---|---|
| not installed | **Install in Terminal**: the first build and the demo database, 10 to 30 minutes. Terminal shows the progress and opens the browser at the end. |
| stopped | **Start**: starts Docker if needed, then the database container and the server, then opens the browser. |
| running | **Open** the web client, or **Stop** the server and the database container. |
| port busy | Another program holds port 8069 (a local Odoo, for example). Stop it and refresh. |

**More...** lists the rest: stop the server but keep the database container running, copy the admin password to
the clipboard (the login is `admin`), show the server log in Console, install an addon from the `addons/`
folder, rebuild after a `git pull`, and details.

The first time the app opens Terminal, macOS asks whether ERPForAll may control Terminal. Allow it, or the
install and rebuild buttons do nothing.

## From a terminal

`erpforall-ctl` works without the app, on macOS and Linux:

```sh
scripts/macos/erpforall-ctl status      # not-installed, stopped, starting, running or port-busy
scripts/macos/erpforall-ctl start
scripts/macos/erpforall-ctl stop        # add --keep-db to leave the database container running
scripts/macos/erpforall-ctl modules install my_addon   # rebuild, stop, install, publish grants, start
scripts/macos/erpforall-ctl help
```

## When something goes wrong

- **"docker was not found" or Docker does not start.** Install Docker Desktop or OrbStack and open it once by
  hand, so it finishes its own setup.
- **"no erp binary".** The build output in `target/` was removed. Choose **Rebuild** in More... .
- **The server stops right after starting.** Look at the server log (More... → Show server log). The same file
  is `~/.erpforall/local/server.log`.
- **Start over.** `scripts/install.sh --reset --yes` removes the server state, the database container and its
  volume. It deletes the demo database.

## Tests

`scripts/macos/tests/test-erpforall-ctl.sh` checks `erpforall-ctl` against a fake checkout and a fake server
(Linux; needs `python3` and `curl`). Nothing is built and no database starts.

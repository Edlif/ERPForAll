<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Accounting report predecessor XML

These unchanged manifest/data files are captured from the exact origin pins in provenance.json.
Regenerate with `python3 -I crates/erp-cli/tests/fixtures/account_reporting_previous/capture.py [receipt]`;
each receipt (`provenance.json`, `provenance-1.10-1.13.json`) is written only by the run that names it.
The script records hashes, actual host/time/tool versions and source revisions.

The upgrade fixture installs these older sources, edits labels and the measure as a manager, then
uses the current module system's upgrade to account_reporting1.6 and reads the actual result as an
accounting reader. Version1.4 was data-only, so its fixture excludes the newer native descriptor;
version1.5 retains the real GL provider. The test uses the current binary/schema composition.
It is data-upgrade evidence and does not claim a previous-binary/schema deployment rehearsal.

Versions 1.10 and 1.13 feed the upgrade tests of `account_off_balance_guard.rs`: an upgrade from each to the current version
must equal a fresh install, keep administrator changes, and stop with a named error where a kept
change cannot fit the new layout.

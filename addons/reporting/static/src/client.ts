/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { registerClientAction } from "@erpforall/client";
import { ReportScreen } from "./report-screen.js";

// The tag is declared on the server by `addons/reporting/src/client_action.rs`; `params.report` names the report.
registerClientAction("reporting.report", { create: async environment => new ReportScreen(environment) });

/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { registerFieldWidget } from "@erpforall/client";
import { issueCostPreview } from "./issue-cost.js";

registerFieldWidget("stock_account.issue_cost_preview", { kinds: ["integer"], render: issueCostPreview });

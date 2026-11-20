/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { registerFieldWidget } from "@erpforall/client";
import { modelList } from "./model-list.js";

registerFieldWidget("ai_base.model_list", { kinds: ["text"], render: modelList });

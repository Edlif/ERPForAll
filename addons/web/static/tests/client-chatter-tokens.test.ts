/*! SPDX-License-Identifier: MIT OR Apache-2.0 */
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { expect, test } from "vitest";

const clientCss = readFileSync(resolve(process.cwd(), "..", "addons/web/static/src/client/client.css"), "utf8");
const threadViewCss = readFileSync(resolve(process.cwd(), "packages/mail-thread-view/src/thread-view.css"), "utf8");
const builtinTs = readFileSync(resolve(process.cwd(), "packages/mail-thread-view/src/builtin.ts"), "utf8");

const normal = (text: string): string => text.replace(/\s+/g, " ").trim();

const rulesIn = (stylesheet: string, selector: string): string[] => {
  const wanted = normal(selector);
  const bodies: string[] = [];
  const css = stylesheet.replace(/\/\*[\s\S]*?\*\//g, "");
  for (const chunk of css.split("}")) {
    const open = chunk.lastIndexOf("{");
    if (open < 0) continue;
    const selectors = chunk.slice(0, open).split("{").pop()!.split(",").map(normal);
    if (selectors.includes(wanted)) bodies.push(chunk.slice(open + 1));
  }
  if (bodies.length === 0) throw new Error(`Stylesheet has no top-level rule for ${selector}`);
  return bodies;
};

const lastIn = (stylesheet: string, selector: string): string => rulesIn(stylesheet, selector).at(-1)!;

/// (partial: chatter and thread view styles use design tokens)
test("chatter and mail attachment thumbnails use token radius instead of raw pixels", () => {
  expect(lastIn(clientCss, ".erp-mail-chatter-file-thumbnail")).toMatch(/border-radius:\s*var\(--radius-control\)/);
  expect(lastIn(clientCss, ".erp-mail-attachment-thumbnail")).toMatch(/border-radius:\s*var\(--radius-control\)/);
  expect(lastIn(threadViewCss, ".erp-mail-attachment-thumbnail")).toMatch(/border-radius:\s*var\(--radius-control\)/);
});

/// (partial: chatter badges and count chips use token radius)
test("chatter duplicate counts and badges use token radius", () => {
  expect(lastIn(clientCss, ".erp-mail-chatter-duplicate-count")).toMatch(/border-radius:\s*var\(--radius-control\)/);
  expect(lastIn(clientCss, ".erp-mail-badge")).toMatch(/border-radius:\s*var\(--radius-control\)/);
  expect(lastIn(threadViewCss, ".erp-mail-badge")).toMatch(/border-radius:\s*var\(--radius-control\)/);
});

/// (partial: mail followers popover uses design token shadow)
test("mail followers popover uses menu shadow token instead of raw rgb", () => {
  const popover = lastIn(clientCss, ".erp-mail-followers-popover");
  expect(popover).toMatch(/box-shadow:\s*var\(--shadow-menu\)/);
  expect(popover).not.toMatch(/rgb\(/);
});

/// (partial: mail thread tracking change renders SVG icon without emoji)
test("mail thread tracking change uses SVG icon without literal arrow glyph", () => {
  expect(builtinTs).not.toContain('" → "');
  expect(builtinTs).toMatch(/glyphNode\(.*"arrow-right"/);
});

test("an internal note is a flat row with a quiet tag, not a filled slab", () => {
  const note = lastIn(clientCss, ".record-form-chatter .erp-mail-chatter .erp-mail-message.is-note");
  expect(note).toMatch(/padding:\s*8px 0/);
  expect(note).toMatch(/background:\s*transparent/);
  const tag = lastIn(clientCss, ".record-form-chatter .erp-mail-chatter .erp-mail-badge");
  expect(tag).toMatch(/text-transform:\s*none/);
  expect(tag).toMatch(/color:\s*var\(--muted\)/);
});

test("writing a note uses the action colour and the plain surface, not the warning colour", () => {
  for (const selector of ['.record-form-chatter .erp-mail-chatter[data-kind="note"] .erp-mail-kind[aria-pressed="true"]', '.record-form-chatter .erp-mail-chatter[data-kind="note"] .erp-mail-send']) {
    expect(lastIn(clientCss, selector)).toMatch(/background:\s*var\(--accent\)/);
  }
  expect(lastIn(clientCss, '.record-form-chatter .erp-mail-chatter[data-kind="note"] .erp-mail-draft')).toMatch(/background:\s*var\(--surface\)/);
});

test("planned activities sit in one panel without a rule down the side, and the form's controls are one height", () => {
  expect(lastIn(clientCss, ".record-form-chatter .erp-mail-activity")).toMatch(/border-inline-start:\s*0/);
  expect(lastIn(clientCss, ".record-form-chatter .erp-mail-activity-rows")).toMatch(/border-radius:\s*var\(--radius-panel\)/);
  expect(lastIn(clientCss, ".record-form-chatter .erp-mail-activity-field > :not(span)")).toMatch(/height:\s*32px/);
  // The state titles of overdue and today lose their filled band: this selector outranks their own rules.
  expect(lastIn(clientCss, ".record-form-chatter .erp-mail-activity-group .erp-mail-activity-group-title")).toMatch(/background:\s*transparent/);
  expect(lastIn(clientCss, '.record-form-chatter .erp-mail-chatter[data-kind="note"] .erp-mail-send:hover')).toMatch(/background:\s*var\(--accent-hover\)/);
});

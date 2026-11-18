/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

import { describe, expect, test } from "vitest";
import {
  hasRule,
  lastRule,
  rulesOf,
  splitSelectors,
} from "./support/stylesheet.js";

describe("support/stylesheet", () => {
  test("splitSelectors splits on commas at parenthesis depth 0 and ignores commas inside :is()", () => {
    expect(splitSelectors(".x, .y :is(.b, .c)")).toEqual([
      ".x",
      ".y :is(.b, .c)",
    ]);
    expect(splitSelectors("  a.foo  ,  b:is(.c, .d, :not(.e, .f))  ")).toEqual([
      "a.foo",
      "b:is(.c, .d, :not(.e, .f))",
    ]);
  });

  test("a selector with :is(.b, .c) is found by rulesOf", () => {
    const css = `
      .a :is(.b, .c) .d {
        color: red;
      }
      .other {
        color: blue;
      }
    `;
    const rules = rulesOf(css, ".a :is(.b, .c) .d");
    expect(rules).toHaveLength(1);
    expect(rules[0]?.body).toContain("color: red;");
    expect(rules[0]?.media).toBeNull();
  });

  test("a rule inside @media has its media text preserved", () => {
    const css = `
      .card {
        padding: 16px;
      }
      @media (prefers-reduced-motion: reduce) {
        .card {
          transition: none;
        }
      }
    `;
    const rules = rulesOf(css, ".card");
    expect(rules).toHaveLength(2);
    expect(rules[0]?.media).toBeNull();
    expect(rules[0]?.body).toContain("padding: 16px;");
    expect(rules[1]?.media).toBe("@media (prefers-reduced-motion: reduce)");
    expect(rules[1]?.body).toContain("transition: none;");
  });

  test("lastRule skips the media twin by default and respects media: any", () => {
    const css = `
      .btn {
        background: blue;
      }
      @media (prefers-reduced-motion: reduce) {
        .btn {
          animation: none;
        }
      }
    `;
    const topOnly = lastRule(css, ".btn");
    expect(topOnly.media).toBeNull();
    expect(topOnly.body).toContain("background: blue;");

    const anyMedia = lastRule(css, ".btn", { media: "any" });
    expect(anyMedia.media).toBe("@media (prefers-reduced-motion: reduce)");
    expect(anyMedia.body).toContain("animation: none;");
  });

  test("lastRule throws an Error naming the selector when no matching rule exists", () => {
    const css = `
      .foo {
        display: block;
      }
    `;
    expect(() => lastRule(css, ".missing-selector")).toThrowError(".missing-selector");
  });

  test("comments containing braces are ignored", () => {
    const css = `
      /* { .fake { color: yellow; } } */
      .real {
        display: flex;
      }
      /* another { comment } */
    `;
    expect(rulesOf(css, ".fake")).toHaveLength(0);
    const realRules = rulesOf(css, ".real");
    expect(realRules).toHaveLength(1);
    expect(realRules[0]?.body).toContain("display: flex;");
  });

  test("hasRule finds patterns across any media or top-level", () => {
    const css = `
      .icon {
        color: black;
      }
      @media (min-width: 600px) {
        .icon {
          font-size: 20px;
        }
      }
    `;
    expect(hasRule(css, ".icon", /font-size:\s*20px/)).toBe(true);
    expect(hasRule(css, ".icon", /color:\s*black/)).toBe(true);
    expect(hasRule(css, ".icon", /display:\s*none/)).toBe(false);
  });
});

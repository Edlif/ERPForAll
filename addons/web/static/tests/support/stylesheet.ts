/*! SPDX-License-Identifier: MIT OR Apache-2.0 */

const normal = (text: string): string => text.replace(/\s+/g, " ").trim();

/**
 * Splits a CSS selector list on commas at parenthesis depth 0 only,
 * so pseudo-classes like `:is(a, b)` or `:not(c, d)` are not broken.
 * Trims and collapses whitespace in each resulting selector.
 */
export const splitSelectors = (list: string): string[] => {
  const result: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < list.length; i++) {
    const ch = list[i];
    if (ch === "(" || ch === "[" || ch === "{") {
      depth++;
    } else if (ch === ")" || ch === "]" || ch === "}") {
      if (depth > 0) depth--;
    } else if (ch === "," && depth === 0) {
      const part = normal(list.slice(start, i));
      if (part) result.push(part);
      start = i + 1;
    }
  }
  const last = normal(list.slice(start));
  if (last) result.push(last);
  return result;
};

export interface MatchedRule {
  body: string;
  media: string | null;
}

/**
 * Strips comments from CSS.
 */
const stripComments = (css: string): string => css.replace(/\/\*[\s\S]*?\*\//g, "");

/**
 * Returns every rule in the stylesheet whose selector list contains `selector`,
 * in file order. For rules inside an at-rule (e.g. `@media`), `media` is the
 * normalized text of the enclosing at-rule prelude; otherwise null.
 */
export const rulesOf = (css: string, selector: string): MatchedRule[] => {
  const wanted = normal(selector);
  const clean = stripComments(css);
  const matched: MatchedRule[] = [];

  let depth = 0;
  let currentAtRule: string | null = null;
  let tokenStart = 0;
  let currentSelectorList: string | null = null;

  for (let i = 0; i < clean.length; i++) {
    const ch = clean[i];
    if (ch === "{") {
      if (depth === 0) {
        const prelude = normal(clean.slice(tokenStart, i));
        if (prelude.startsWith("@")) {
          currentAtRule = prelude;
          tokenStart = i + 1;
        } else {
          currentAtRule = null;
          currentSelectorList = prelude;
          tokenStart = i + 1;
        }
      } else if (depth === 1 && currentAtRule !== null) {
        currentSelectorList = normal(clean.slice(tokenStart, i));
        tokenStart = i + 1;
      }
      depth++;
    } else if (ch === "}") {
      depth--;
      if (depth === 0) {
        if (currentAtRule === null) {
          // Top-level rule finished
          if (currentSelectorList) {
            const selectors = splitSelectors(currentSelectorList);
            if (selectors.includes(wanted)) {
              matched.push({ body: clean.slice(tokenStart, i), media: null });
            }
          }
          currentSelectorList = null;
        } else {
          // At-rule finished
          currentAtRule = null;
        }
        tokenStart = i + 1;
      } else if (depth === 1 && currentAtRule !== null) {
        // Nested rule inside at-rule finished
        if (currentSelectorList) {
          const selectors = splitSelectors(currentSelectorList);
          if (selectors.includes(wanted)) {
            matched.push({ body: clean.slice(tokenStart, i), media: currentAtRule });
          }
        }
        currentSelectorList = null;
        tokenStart = i + 1;
      }
    }
  }

  return matched;
};

export interface LastRuleOptions {
  media?: "none" | "any";
}

/**
 * Returns the last rule matching `selector`.
 * By default (`media: "none"`), only matches rules that are NOT inside an at-rule.
 * If `media: "any"`, returns the last rule across both top-level and at-rules.
 * Throws an Error naming the selector if no matching rule is found.
 */
export const lastRule = (
  css: string,
  selector: string,
  options?: LastRuleOptions
): MatchedRule => {
  const mode = options?.media ?? "none";
  const all = rulesOf(css, selector);
  const candidates = mode === "none" ? all.filter((r) => r.media === null) : all;
  if (candidates.length === 0) {
    throw new Error(`Stylesheet has no rule for ${selector}`);
  }
  return candidates[candidates.length - 1]!;
};

/**
 * Checks whether any rule for `selector` (in any media or top-level) matches `pattern`.
 */
export const hasRule = (css: string, selector: string, pattern: RegExp): boolean => {
  const all = rulesOf(css, selector);
  return all.some((r) => pattern.test(r.body));
};

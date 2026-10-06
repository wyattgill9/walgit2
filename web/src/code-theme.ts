/** Options shared by every @pierre/diffs view (blob + diff). Both syntax themes are rendered
 * and `themeType: "system"` lets the page's `color-scheme` (set by the theme toggle) pick one,
 * so switching needs no re-render. The library sets `color-scheme: light dark` on its host,
 * which would follow the OS instead of the toggle, hence `color-scheme: inherit`. The surface comes from the page's greyscale tokens, which
 * inherit into the shadow DOM; the theme sets --diffs-bg on :host, so it is overridden in the
 * library's `unsafe` layer, the only one that outranks it. */
export const CODE = {
  theme: { dark: "one-dark-pro", light: "one-light" },
  themeType: "system",
  overflow: "scroll",
  unsafeCSS: ":host { color-scheme: inherit; --diffs-bg: var(--l1); --diffs-bg-separator-override: var(--l2); --diffs-fg-number-override: var(--l6); }",
} as const;

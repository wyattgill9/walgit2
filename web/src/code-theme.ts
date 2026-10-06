/** Options shared by every @pierre/diffs view (blob + diff): One Dark tokens on the page's
 * greyscale surface. The theme sets its background on :host, so it is overridden in the
 * library's `unsafe` layer, the only one that outranks it. */
export const CODE = {
  theme: "one-dark-pro",
  themeType: "dark",
  overflow: "scroll",
  unsafeCSS: ":host { --diffs-bg: #0c0c0c; --diffs-bg-separator-override: #131313; --diffs-fg-number-override: #4a4a4a; }",
} as const;

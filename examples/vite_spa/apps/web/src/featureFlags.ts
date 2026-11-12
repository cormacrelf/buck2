// Feature switches, read once at startup.
//
// HMR note: main.tsx imports this module and main.tsx accepts no updates, so
// an edit here reaches the top of the graph and Vite does a full page reload.
// That's deliberate: flags decide which routes exist, and a hot swap would
// leave the router built from the old values. Contrast with the components,
// whose edits are swapped in place by React Fast Refresh with state kept.

export interface FeatureFlags {
  /** Elevation chart on the tracks page. */
  elevationProfile: boolean;
  /** The species checklist route. */
  speciesChecklist: boolean;
  /** Show build details in the footer. */
  buildBadge: boolean;
}

export const flags: Readonly<FeatureFlags> = Object.freeze({
  elevationProfile: true,
  speciesChecklist: true,
  buildBadge: true,
});

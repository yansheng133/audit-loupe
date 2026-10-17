// A global product (Rancher's own store, not tied to one cluster): the change map spans
// every cluster that sends audit data. Uses the stable product DSL rather than the
// experimental product registration API.
export const PRODUCT = 'audit-loupe';
export const BLANK_CLUSTER = '_';
export const PAGES = ['changes', 'access', 'coverage'];
export const PAGE = 'changes';
export const routeName = (page) => `${ PRODUCT }-c-cluster-${ page }`;
export const ROUTE = routeName(PAGE);

export function init($plugin, store) {
  const { product, virtualType, basicType } = $plugin.DSL(store, PRODUCT);

  product({
    icon:                'search',
    inStore:             'management',
    weight:              90,
    showClusterSwitcher: false,
    to:                  { name: ROUTE, params: { product: PRODUCT, cluster: BLANK_CLUSTER } },
  });

  PAGES.forEach((page, i) => {
    virtualType({
      labelKey: `auditLoupe.nav.${ page }`,
      name:     page,
      weight:   100 - i,
      route:    { name: routeName(page), params: { product: PRODUCT, cluster: BLANK_CLUSTER } },
    });
  });

  basicType(PAGES);
}

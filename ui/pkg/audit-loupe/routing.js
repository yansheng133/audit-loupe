import ChangeMap from './pages/ChangeMap.vue';
import AccessReview from './pages/AccessReview.vue';
import Coverage from './pages/Coverage.vue';
import Rules from './pages/Rules.vue';
import { PRODUCT, BLANK_CLUSTER, routeName } from './product';

const page = (name, component) => ({
  name:      routeName(name),
  path:      `/${ PRODUCT }/c/:cluster/${ name }`,
  component,
  meta:      {
    product: PRODUCT, cluster: BLANK_CLUSTER, pkg: PRODUCT
  },
});

export default [
  page('changes', ChangeMap),
  page('access', AccessReview),
  page('coverage', Coverage),
  page('rules', Rules),
];

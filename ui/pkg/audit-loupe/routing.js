import ChangeMap from './pages/ChangeMap.vue';
import {
  PRODUCT, BLANK_CLUSTER, PAGE, ROUTE
} from './product';

export default [
  {
    name:      ROUTE,
    path:      `/${ PRODUCT }/c/:cluster/${ PAGE }`,
    component: ChangeMap,
    meta:      {
      product: PRODUCT, cluster: BLANK_CLUSTER, pkg: PRODUCT
    },
  },
];

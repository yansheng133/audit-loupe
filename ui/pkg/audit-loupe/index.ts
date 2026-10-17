import { importTypes } from '@rancher/auto-import';
import { IPlugin, CardLocation } from '@shell/core/types';
import routes from './routing';

// Audit Loupe: a global "change map" page across clusters, plus a card on each cluster's
// dashboard that opens it with that cluster selected.
export default function(plugin: IPlugin): void {
  importTypes(plugin);
  plugin.metadata = require('./package.json');

  plugin.addProduct(require('./product'));
  plugin.addRoutes(routes);

  plugin.addCard(CardLocation.CLUSTER_DASHBOARD_CARD, {}, {
    label:     'Audit Loupe',
    component: () => import('./components/ClusterCard.vue'),
  });
}

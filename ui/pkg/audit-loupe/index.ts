import { importTypes } from '@rancher/auto-import';
import { IPlugin } from '@shell/core/types';

// Adds an "Audit Loupe" group to each cluster's Explorer side menu.
export default function(plugin: IPlugin): void {
  importTypes(plugin);
  plugin.metadata = require('./package.json');

  // 'explorer' is StandardProductNames.EXPLORER; the literal avoids bundling shell internals.
  plugin.extendProduct('explorer', [{
    name:     'audit-loupe',
    labelKey: 'auditLoupe.nav.group',
    sideMenu: {
      weight:   -10,
      default:  'audit-timeline',
      children: [
        {
          name: 'audit-timeline', labelKey: 'auditLoupe.nav.timeline', component: () => import('./pages/Timeline.vue'), sideMenu: { weight: 3 }
        },
        {
          name: 'audit-objects', labelKey: 'auditLoupe.nav.objects', component: () => import('./pages/Objects.vue'), sideMenu: { weight: 2 }
        },
        {
          name: 'audit-alerts', labelKey: 'auditLoupe.nav.alerts', component: () => import('./pages/Alerts.vue'), sideMenu: { weight: 1 }
        },
      ],
    },
  }]);
}

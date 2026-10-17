<script>
import { PRODUCT, BLANK_CLUSTER, ROUTE } from '../product';
import { texts } from '../lib/model';

// On each cluster's dashboard: a shortcut into the change map with that cluster selected.
// Rancher has no extension point for links in another product's side menu, so a card it is.
export default {
  name: 'AuditLoupeClusterCard',

  computed: {
    T() {
      return texts(this.$store);
    },
    to() {
      return {
        name:   ROUTE,
        params: { product: PRODUCT, cluster: BLANK_CLUSTER },
        query:  { cluster: this.$route.params.cluster },
      };
    },
  },
};
</script>

<template>
  <div class="audit-loupe-card">
    <span>Audit Loupe：{{ T.evidenceHint }}</span>
    <router-link :to="to">
      {{ T.title }} →
    </router-link>
  </div>
</template>

<style scoped>
.audit-loupe-card { display: flex; flex-wrap: wrap; gap: 8px 16px; align-items: center; justify-content: space-between; padding: 12px 16px; border: 1px solid var(--border); border-radius: 6px; }
</style>

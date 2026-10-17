<script>
import {
  getJson, dict, fmtTime, objLabel
} from '../lib/api';

const SEVERITY = {
  namespace_deleted: 'high', rbac_changed: 'medium', pod_exec: 'medium', bypassed_rancher: 'medium', secret_read: 'low', off_hours: 'low', forbidden: 'low'
};

export default {
  name: 'AuditAlerts',

  data() {
    return {
      t:       dict(this.$store),
      hours:   168,
      rows:    [],
      loading: false,
      error:   null,
    };
  },

  computed: {
    cluster() {
      return this.$route.params.cluster;
    },
  },

  watch: { hours: 'load' },

  mounted() {
    this.load();
  },

  methods: {
    fmtTime,
    objLabel,

    async load() {
      this.loading = true;
      this.error = null;
      try {
        this.rows = await getJson(this.cluster, '/api/alerts', { hours: this.hours, limit: 500 });
      } catch (e) {
        this.error = e.status === 404 || e.status === 403 || e.status === 503 ? this.t.backendMissing : `${ this.t.error }: ${ e.message }`;
      } finally {
        this.loading = false;
      }
    },

    sev(rule) {
      return SEVERITY[rule] || 'low';
    },

    timelineLink(actor) {
      return {
        name: 'c-cluster-explorer-audit-timeline', params: { cluster: this.cluster }, query: { actor, hours: this.hours }
      };
    },
  },
};
</script>

<template>
  <div class="at-page">
    <h1>{{ t.alerts }}</h1>
    <div class="at-filters">
      <label>{{ t.range }}
        <select v-model.number="hours">
          <option :value="24">{{ t.h24 }}</option>
          <option :value="168">{{ t.h168 }}</option>
          <option :value="720">{{ t.h720 }}</option>
        </select>
      </label>
    </div>

    <div
      v-if="error"
      class="banner bg-warning"
    >{{ error }}</div>
    <div v-else-if="loading">{{ t.loading }}</div>
    <div v-else-if="!rows.length">{{ t.noData }}</div>

    <table
      v-else
      class="at-table"
    >
      <thead>
        <tr>
          <th>{{ t.time }}</th>
          <th>{{ t.rule }}</th>
          <th>{{ t.actor }}</th>
          <th>{{ t.action }}</th>
          <th>{{ t.object }}</th>
          <th>{{ t.result }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="e in rows"
          :key="e.id"
        >
          <td class="nowrap">{{ fmtTime(e.ts) }}</td>
          <td>
            <span
              v-for="r in e.alerts"
              :key="r"
              :class="['flag', sev(r)]"
            >{{ t.sev[sev(r)] }}・{{ t.rules[r] || r }}</span>
          </td>
          <td><router-link :to="timelineLink(e.actor)">{{ e.actor }}</router-link></td>
          <td>{{ e.subresource ? e.verb + ' (' + e.subresource + ')' : e.verb }}</td>
          <td>{{ objLabel(e) }}</td>
          <td>{{ e.code }}</td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style lang="scss" scoped>
@import '../components/page.scss';
</style>

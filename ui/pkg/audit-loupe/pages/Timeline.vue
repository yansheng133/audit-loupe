<script>
import ChangeList from '../components/ChangeList.vue';
import {
  getJson, dict, fmtTime, fmtValue, objLabel
} from '../lib/api';

export default {
  name:       'AuditTimeline',
  components: { ChangeList },

  data() {
    return {
      t:       dict(this.$store),
      actors:  [],
      rows:    [],
      actor:   this.$route.query.actor || '',
      hours:   Number(this.$route.query.hours || 24),
      showAll: false,
      loading: false,
      error:   null,
      open:    {},
    };
  },

  computed: {
    cluster() {
      return this.$route.params.cluster;
    },
  },

  watch: {
    '$route.query.actor'(v) {
      this.actor = v || '';
    },
    actor:   'load',
    hours:   'load',
    showAll: 'load',
  },

  mounted() {
    this.load();
  },

  methods: {
    fmtTime,
    fmtValue,
    objLabel,

    async load() {
      this.loading = true;
      this.error = null;
      try {
        const [actors, rows] = await Promise.all([
          getJson(this.cluster, '/api/actors', { hours: this.hours }),
          getJson(this.cluster, '/api/timeline', {
            hours: this.hours, actor: this.actor, all: this.showAll ? 1 : undefined, limit: 500
          }),
        ]);

        this.actors = actors.filter((a) => this.showAll || a.human);
        this.rows = rows;
      } catch (e) {
        this.error = e.status === 404 || e.status === 403 || e.status === 503 ? this.t.backendMissing : `${ this.t.error }: ${ e.message }`;
      } finally {
        this.loading = false;
      }
    },

    viaLabel(v) {
      return {
        rancher: this.t.viaRancher, direct: this.t.viaDirect, ace: this.t.viaAce
      }[v] || this.t.viaImpersonation;
    },

    action(e) {
      return e.subresource ? `${ e.verb } (${ e.subresource })` : e.verb;
    },

    historyLink(e) {
      return {
        name:   'c-cluster-explorer-audit-objects',
        params: { cluster: this.cluster },
        query:  {
          c: e.cluster, group: e.api_group, resource: e.resource, namespace: e.namespace, name: e.name
        },
      };
    },

    toggle(id) {
      this.open = { ...this.open, [id]: !this.open[id] };
    },
  },
};
</script>

<template>
  <div class="at-page">
    <h1>{{ t.timeline }}</h1>

    <div class="at-filters">
      <label>{{ t.actor }}
        <select v-model="actor">
          <option value="">{{ t.allActors }}</option>
          <option
            v-for="a in actors"
            :key="a.actor"
            :value="a.actor"
          >{{ a.actor }}（{{ a.writes }}{{ a.alerts ? ' / ⚠ ' + a.alerts : '' }}）</option>
        </select>
      </label>
      <label>{{ t.range }}
        <select v-model.number="hours">
          <option :value="24">{{ t.h24 }}</option>
          <option :value="72">{{ t.h72 }}</option>
          <option :value="168">{{ t.h168 }}</option>
          <option :value="720">{{ t.h720 }}</option>
        </select>
      </label>
      <label class="check"><input
        v-model="showAll"
        type="checkbox"
      > {{ t.showAll }}</label>
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
          <th>{{ t.actor }}</th>
          <th>{{ t.via }}</th>
          <th>{{ t.action }}</th>
          <th>{{ t.object }}</th>
          <th>{{ t.result }}</th>
          <th>{{ t.changes }}</th>
          <th>{{ t.flags }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="e in rows"
          :key="e.id"
          :class="{ alert: e.alerts.length }"
        >
          <td class="nowrap">{{ fmtTime(e.ts) }}</td>
          <td>
            <a
              href="#"
              @click.prevent="actor = e.actor"
            >{{ e.actor }}</a>
            <div
              v-if="e.principal && e.principal !== e.actor"
              class="sub"
            >{{ e.principal }}</div>
          </td>
          <td><span :class="['via', e.via]">{{ viaLabel(e.via) }}</span></td>
          <td>{{ action(e) }}</td>
          <td>
            <router-link
              v-if="e.name"
              :to="historyLink(e)"
            >{{ objLabel(e) }}</router-link>
            <span v-else>{{ objLabel(e) }}</span>
          </td>
          <td :class="(e.code >= 200 && e.code < 300) || e.code === 101 ? 'text-success' : 'text-error'">{{ e.code }}</td>
          <td>
            <ChangeList
              v-if="e.changes && e.changes.length"
              :changes="e.changes"
            />
            <template v-else-if="e.request_object">
              <a
                href="#"
                @click.prevent="toggle(e.id)"
              >{{ t.requestBody }}</a>
              <pre v-if="open[e.id]">{{ JSON.stringify(e.request_object, null, 2) }}</pre>
            </template>
          </td>
          <td>
            <span
              v-for="r in e.alerts"
              :key="r"
              class="flag"
            >{{ t.rules[r] || r }}</span>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>

<style lang="scss" scoped>
@import '../components/page.scss';
</style>

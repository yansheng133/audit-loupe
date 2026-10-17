<script>
import ChangeList from '../components/ChangeList.vue';
import { getJson, dict, fmtTime } from '../lib/api';

export default {
  name:       'AuditObjects',
  components: { ChangeList },

  data() {
    return {
      t:        dict(this.$store),
      q:        '',
      objects:  [],
      versions: [],
      selected: null,
      loading:  false,
      error:    null,
      open:     {},
    };
  },

  computed: {
    cluster() {
      return this.$route.params.cluster;
    },
  },

  watch: {
    q() {
      clearTimeout(this.timer);
      this.timer = setTimeout(this.loadObjects, 300);
    },
    '$route.query': 'fromQuery',
  },

  mounted() {
    this.loadObjects();
    this.fromQuery();
  },

  methods: {
    fmtTime,

    async loadObjects() {
      this.error = null;
      try {
        this.error = null;
        this.objects = await getJson(this.cluster, '/api/objects', { q: this.q, limit: 300 });
      } catch (e) {
        this.error = e.status === 404 || e.status === 403 || e.status === 503 ? this.t.backendMissing : `${ this.t.error }: ${ e.message }`;
      }
    },

    fromQuery() {
      const q = this.$route.query;

      if (q.resource && q.name) {
        this.pick({
          api_group: q.group || '', resource: q.resource, namespace: q.namespace || '', name: q.name
        });
      }
    },

    async pick(o) {
      this.selected = o;
      this.loading = true;
      this.versions = [];
      this.error = null;
      try {
        this.versions = await getJson(this.cluster, '/api/history', {
          cluster: o.cluster || this.$route.query.c || undefined,
          group:   o.api_group,
          resource: o.resource,
          namespace: o.namespace,
          name:    o.name,
        });
      } catch (e) {
        this.error = `${ this.t.error }: ${ e.message }`;
      } finally {
        this.loading = false;
      }
    },

    isSelected(o) {
      const s = this.selected;

      return s && s.resource === o.resource && s.namespace === o.namespace && s.name === o.name && s.api_group === o.api_group;
    },

    who(v) {
      if (v.actor) {
        return v.actor;
      }
      if (v.change === 'baseline') {
        return '';
      }
      if (v.change === 'modified_unseen' || v.change === 'added_unseen') {
        return this.t.unseen;
      }

      return this.t.unattributed;
    },

    changeLabel(c) {
      const map = { modified_unseen: 'modified', added_unseen: 'addedUnseen' };

      return this.t[map[c] || c] || c;
    },

    toggle(id) {
      this.open = { ...this.open, [id]: !this.open[id] };
    },
  },
};
</script>

<template>
  <div class="at-page">
    <h1>{{ t.objects }}</h1>
    <div
      v-if="error"
      class="banner bg-warning"
    >{{ error }}</div>

    <div class="at-split">
      <div class="left">
        <div class="at-filters">
          <input
            v-model="q"
            type="text"
            :placeholder="t.search"
          >
        </div>
        <table class="at-table">
          <thead>
            <tr>
              <th>{{ t.object }}</th>
              <th>{{ t.versions }}</th>
              <th>{{ t.lastChange }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="o in objects"
              :key="o.cluster + o.api_group + o.resource + o.namespace + o.name"
              :class="{ selected: isSelected(o) }"
            >
              <td>
                <a
                  href="#"
                  @click.prevent="pick(o)"
                >{{ o.kind }} {{ o.namespace ? o.namespace + '/' : '' }}{{ o.name }}</a>
              </td>
              <td>{{ o.versions }}</td>
              <td class="nowrap">{{ fmtTime(o.last_change) }}</td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="right">
        <div v-if="!selected">{{ t.pickObject }}</div>
        <template v-else>
          <h2>{{ selected.resource }} {{ selected.namespace ? selected.namespace + '/' : '' }}{{ selected.name }}</h2>
          <div v-if="loading">{{ t.loading }}</div>
          <ol class="at-history">
            <li
              v-for="v in versions"
              :key="v.id"
              :class="'c-' + v.change"
            >
              <div class="head">
                <span class="when">{{ fmtTime(v.observed) }}</span>
                <span class="what">{{ changeLabel(v.change) }}</span>
                <span :class="['who', { muted: !v.actor }]">{{ who(v) }}</span>
                <span
                  v-if="v.via === 'direct'"
                  class="via direct"
                >{{ t.viaDirect }}</span>
              </div>
              <ChangeList
                v-if="v.changes.length"
                :changes="v.changes"
              />
              <a
                href="#"
                class="spec"
                @click.prevent="toggle(v.id)"
              >{{ open[v.id] ? t.hideSpec : t.showSpec }}</a>
              <pre v-if="open[v.id]">{{ JSON.stringify(v.body, null, 2) }}</pre>
            </li>
          </ol>
        </template>
      </div>
    </div>
  </div>
</template>

<style lang="scss" scoped>
@import '../components/page.scss';
.at-split {
  display: grid;
  grid-template-columns: minmax(320px, 2fr) 3fr;
  gap: 24px;
}
.at-history {
  list-style: none; padding: 0; margin: 0;
  li { border-left: 3px solid var(--border); padding: 8px 12px; margin-bottom: 12px; }
  li.c-modified, li.c-added { border-left-color: var(--primary); }
  li.c-deleted, li.c-deleting { border-left-color: var(--error); }
  li.c-modified_unseen, li.c-added_unseen { border-left-color: var(--warning); }
  .head { display: flex; gap: 12px; align-items: baseline; margin-bottom: 4px; flex-wrap: wrap; }
  .what { font-weight: 600; }
  .who.muted { color: var(--muted); }
  .spec { font-size: 12px; }
  pre { max-height: 360px; overflow: auto; font-size: 12px; }
}
</style>

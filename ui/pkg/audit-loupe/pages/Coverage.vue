<script>
import { getJson } from '../lib/api';
import { texts, fmtDate } from '../lib/model';

// Is collection working? Per cluster: each collector's last check-in and own report, gaps
// in the last 7 days; the record chain's verification; forwarding to the SIEM.
export default {
  name: 'AuditLoupeCoverage',

  data() {
    return {
      T: texts(this.$store), rows: [], chain: [], forwarding: [], loading: false, error: null,
    };
  },

  computed: {
    C() {
      return this.T.cov;
    },
  },

  mounted() {
    this.load();
  },

  methods: {
    fmtDate,
    clusterName(id) {
      try {
        return this.$store.getters['management/byId']('management.cattle.io.cluster', id)?.nameDisplay || id;
      } catch (e) {
        return id;
      }
    },
    details(d) {
      return Object.entries(d || {}).map(([k, v]) => `${ k }: ${ typeof v === 'object' ? JSON.stringify(v) : v }`).join('・');
    },
    chainOf(c) {
      return this.chain.find((x) => x.cluster === c);
    },
    async load() {
      this.loading = true;
      this.error = null;
      try {
        await this.$store.dispatch('management/findAll', { type: 'management.cattle.io.cluster' }).catch(() => {});
        [this.rows, this.chain, this.forwarding] = await Promise.all([
          getJson('/api/coverage', { hours: 168 }),
          getJson('/api/integrity'),
          getJson('/api/forwarding'),
        ]);
      } catch (e) {
        this.error = [403, 404, 503].includes(e.status) ? this.T.backendMissing : `${ this.T.error }: ${ e.message }`;
      } finally {
        this.loading = false;
      }
    },
  },
};
</script>

<template>
  <div class="al al-page">
    <div class="topbar">
      <span class="crumb">Audit Loupe ／ {{ T.crumb }}</span>
      <h1>{{ C.title }}</h1>
      <div class="filters">
        <button
          class="btn-al"
          type="button"
          @click="load"
        >
          {{ C.refresh }}
        </button>
      </div>
    </div>
    <div
      v-if="error"
      class="banner-al"
    >
      {{ error }}
    </div>
    <div
      v-if="loading"
      class="quiet"
    >
      {{ T.loading }}
    </div>

    <section
      v-for="r in rows"
      :key="r.cluster"
      class="col cov-card"
    >
      <div class="col-head">
        <h2>{{ clusterName(r.cluster) }}</h2>
        <span class="muted">{{ r.cluster }}</span>
      </div>
      <div class="detail">
        <table class="tbl">
          <thead>
            <tr>
              <th>{{ C.collector }}</th><th>{{ C.instance }}</th><th>{{ C.lastSeen }}</th><th>{{ C.status }}</th><th>{{ C.report }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="c in r.collectors"
              :key="c.collector + c.instance"
            >
              <td>{{ C.kinds[c.collector] || c.collector }}</td>
              <td>{{ c.instance }}</td>
              <td class="num">
                {{ fmtDate(c.lastMs) }}
              </td>
              <td>
                <span
                  class="pill"
                  :class="c.stale ? 'deleted' : 'added'"
                >{{ c.stale ? C.silent : C.ok }}</span>
              </td>
              <td class="muted">
                {{ details(c.details) }}
              </td>
            </tr>
          </tbody>
        </table>
        <div class="sub">
          <span>{{ C.gaps }}</span><span>{{ r.gaps.length }}</span>
        </div>
        <div
          v-for="(g, i) in r.gaps"
          :key="i"
          class="quiet"
        >
          {{ C.kinds[g.collector] || g.collector }}（{{ g.instance }}）：{{ fmtDate(g.fromMs) }} → {{ g.toMs ? fmtDate(g.toMs) : C.stillSilent }}
        </div>
        <div
          v-if="chainOf(r.cluster)"
          class="sub"
        >
          <span>{{ C.chain }}</span>
          <span :class="chainOf(r.cluster).first_bad_id ? 'pill deleted' : 'pill added'">
            {{ chainOf(r.cluster).first_bad_id ? C.chainBroken(chainOf(r.cluster).first_bad_id) : C.chainOk(chainOf(r.cluster).verified) }}
          </span>
        </div>
      </div>
    </section>

    <section class="col cov-card">
      <div class="col-head">
        <h2>{{ C.forwarding }}</h2>
      </div>
      <div class="detail">
        <div
          v-if="!forwarding.length"
          class="quiet"
        >
          {{ C.noForwarding }}
        </div>
        <table
          v-else
          class="tbl"
        >
          <tbody>
            <tr
              v-for="f in forwarding"
              :key="f.target"
            >
              <td>{{ f.target }}</td>
              <td class="num">
                {{ f.lastOkMs ? fmtDate(f.lastOkMs) : '—' }}
              </td>
              <td>{{ C.pending(f.pending) }}</td>
              <td class="muted">
                {{ f.lastError || '' }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </section>
  </div>
</template>

<style lang="scss">
@import '../styles/change-map.scss';
.al.al-page { height: auto; display: flex; flex-direction: column; }
.al .cov-card { flex: none; }
.al .tbl { width: 100%; border-collapse: collapse; font-size: 12.5px; }
.al .tbl th { text-align: left; font-weight: 700; color: var(--al-muted); border-bottom: 1px solid var(--al-border); padding: 4px 6px; }
.al .tbl td { border-bottom: 1px solid var(--al-border); padding: 4px 6px; vertical-align: top; }
</style>

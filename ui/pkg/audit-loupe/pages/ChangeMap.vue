<script>
import SidePanel from '../components/SidePanel.vue';
import TopologyMap from '../components/TopologyMap.vue';
import EvidencePanel from '../components/EvidencePanel.vue';
import TimeAxis from '../components/TimeAxis.vue';
import RecomputeBanner from '../components/RecomputeBanner.vue';
import {
  getJson, backendCluster, setBackendCluster
} from '../lib/api';
import {
  texts, buildModel, coverageOf, dayStart, fmtMin, isRisky, pad
} from '../lib/model';

const LIMIT = 2000;

function isoDay(d) {
  return `${ d.getFullYear() }-${ pad(d.getMonth() + 1) }-${ pad(d.getDate()) }`;
}

export default {
  name:       'AuditLoupeChangeMap',
  components: {
    RecomputeBanner,
    SidePanel, TopologyMap, EvidencePanel, TimeAxis
  },

  data() {
    const q = this.$route.query || {};

    return {
      T:          texts(this.$store),
      day:        q.day || isoDay(new Date()),
      a:          0,
      b:          1440,
      cluster:    q.cluster || '',
      project:    '',
      ns:         '',
      actor:      '',
      lifetime:   '',
      sys:        false,
      sel:        null,
      selEvent:   null,
      related:    [],
      model:      { OBJ: {}, ACTORS: {}, EVENTS: [] },
      stories:    [],
      clusters:   [],
      coverageRows: [],
      work:       [9, 18],
      loading:    false,
      error:      null,
      missing:    false,
      truncated:  false,
      backend:    backendCluster(),
      backendIn:  backendCluster(),
    };
  },

  computed: {
    OBJ() {
      return this.model.OBJ;
    },
    ACTORS() {
      return this.model.ACTORS;
    },
    EVENTS() {
      return this.model.EVENTS;
    },
    lifetimeRange() {
      if (!this.lifetime || !this.actor) {
        return null;
      }
      const l = (this.ACTORS[this.actor]?.lifetimes || []).find((x) => x.uid === this.lifetime);

      return l ? [l.created_ms, l.deleted_ms || Number.MAX_SAFE_INTEGER] : null;
    },
    // Scope: cluster, project, namespace, system components. Not time or person.
    scoped() {
      return this.EVENTS.filter((ev) => {
        const o = this.OBJ[ev.obj];

        if (!this.sys && this.ACTORS[ev.actor].system) {
          return false;
        }
        if (this.cluster && o.cluster !== this.cluster) {
          return false;
        }
        if (this.project && o.project !== this.project) {
          return false;
        }

        return !this.ns || o.ns === this.ns;
      });
    },
    personScoped() {
      return this.scoped.filter((ev) => this.matchesPerson(ev));
    },
    visible() {
      return this.personScoped.filter((ev) => this.inRange(ev));
    },
    inRangeScoped() {
      return this.scoped.filter((ev) => this.inRange(ev));
    },
    clusterOptions() {
      return [...new Set(Object.values(this.OBJ).map((o) => o.cluster))].sort().map((c) => [c, this.clusterName(c) || c]);
    },
    projectOptions() {
      const objs = Object.values(this.OBJ).filter((o) => !this.cluster || o.cluster === this.cluster);

      return [...new Set(objs.map((o) => o.project))].sort();
    },
    nsOptions() {
      const objs = Object.values(this.OBJ).filter((o) => (!this.cluster || o.cluster === this.cluster) && (!this.project || o.project === this.project));

      return [...new Set(objs.map((o) => o.ns))].sort();
    },
    actorOptions() {
      return Object.values(this.ACTORS).filter((a) => this.sys || !a.system).map((a) => [a.key, a.role ? `${ a.label }（${ a.role }）` : a.label]);
    },
    coverage() {
      const out = {};

      this.clusterOptions.forEach(([c]) => {
        out[c] = coverageOf(this.clusters.find((x) => x.cluster === c), this.T, this.coverageRows.find((x) => x.cluster === c));
      });

      return out;
    },
    // Server-side stories, kept when at least one of their events is visible now.
    visibleStories() {
      const vis = new Set(this.visible.map((e) => e.id));
      const byId = new Map(this.EVENTS.map((e) => [e.id, e]));

      return this.stories
        .map((s) => ({ ...s, evs: s.events.map((e) => byId.get(`e${ e.id }`)).filter(Boolean) }))
        .filter((s) => s.evs.some((e) => vis.has(e.id)));
    },
    dayLabel() {
      return this.day;
    },
  },

  watch: {
    day() {
      this.sel = null;
      this.selEvent = null;
      this.load();
    },
    cluster() {
      this.project = '';
      this.ns = '';
      this.sel = null;
    },
    project() {
      this.ns = '';
      this.sel = null;
    },
    ns() {
      this.sel = null;
    },
    actor() {
      this.lifetime = '';
    },
    sel(k) {
      this.loadRelated(k);
    },
  },

  mounted() {
    this.load();
  },

  methods: {
    fmtMin,

    inRange(ev) {
      return ev.t >= this.a && ev.t <= this.b;
    },

    matchesPerson(ev) {
      if (!this.actor) {
        return true;
      }
      if (ev.actor !== this.actor) {
        return false;
      }
      const r = this.lifetimeRange;

      return !r || (ev.ts >= r[0] && ev.ts < r[1]);
    },

    clusterName(id) {
      try {
        const c = this.$store.getters['management/byId']('management.cattle.io.cluster', id);

        return c?.nameDisplay || c?.spec?.displayName || '';
      } catch (e) {
        return '';
      }
    },

    projectName(cluster, pid) {
      try {
        const p = this.$store.getters['management/byId']('management.cattle.io.project', `${ cluster }/${ pid }`);

        return p?.nameDisplay || p?.spec?.displayName || '';
      } catch (e) {
        return '';
      }
    },

    async load() {
      this.loading = true;
      this.error = null;
      this.missing = false;
      const from = dayStart(`${ this.day }T00:00:00`);
      const to = new Date(from.getTime() + 86400000);
      const range = { from: from.toISOString(), to: to.toISOString() };

      try {
        // Cluster and project names come from Rancher itself.
        await Promise.all([
          this.$store.dispatch('management/findAll', { type: 'management.cattle.io.cluster' }).catch(() => {}),
          this.$store.dispatch('management/findAll', { type: 'management.cattle.io.project' }).catch(() => {}),
        ]);
        const [info, rows, unattributed, actors, stories, clusters, nsProjects, coverageRows] = await Promise.all([
          getJson('/api/info'),
          getJson('/api/timeline', { ...range, all: 1, limit: LIMIT }),
          getJson('/api/unattributed', range),
          getJson('/api/actors', range),
          getJson('/api/stories', range),
          getJson('/api/clusters', range),
          getJson('/api/projects'),
          getJson('/api/coverage', range).catch(() => []),
        ]);
        const [ws, we] = String(info.workHours || '9-18').split('-').map(Number);

        this.work = [ws || 9, we || 18];
        this.truncated = rows.length >= LIMIT;
        this.model = buildModel({
          rows, unattributed, actors, nsProjects, day: from, T: this.T, clusterName: this.clusterName, projectName: this.projectName,
        });
        this.stories = stories;
        this.clusters = clusters;
        this.coverageRows = coverageRows;
        this.openFirstStory();
      } catch (e) {
        console.error('audit-loupe: load failed', e); // eslint-disable-line no-console
        if ([403, 404, 503].includes(e.status)) {
          this.missing = true;
        } else {
          this.error = `${ this.T.error }: ${ e.message }`;
        }
      } finally {
        this.loading = false;
      }
    },

    async loadRelated(k) {
      this.related = [];
      const o = k && this.OBJ[k];

      if (!o) {
        return;
      }
      try {
        const rows = await getJson('/api/related', {
          cluster: o.cluster, kind: o.kind, namespace: o.namespace, name: o.name
        });

        if (this.sel === k) {
          this.related = rows.map((r) => ({ ...r, key: `${ o.cluster }|${ r.kind }|${ r.namespace }|${ r.name }` }));
        }
      } catch (e) {
        this.related = [];
      }
    },

    // Open on the most serious item so every column has something to show.
    openFirstStory() {
      const s = this.visibleStories[0];
      const ev = s ? s.evs[0] : [...this.visible].reverse().find(isRisky);

      if (ev) {
        this.select(ev.obj, ev.id);
      }
    },

    select(k, evId) {
      if (!k) {
        this.sel = null;
        this.selEvent = null;

        return;
      }
      const evs = this.scoped.filter((e) => e.obj === k);

      this.sel = k;
      this.selEvent = evId || ([...evs].reverse().find((e) => this.inRange(e) && this.matchesPerson(e)) || evs[evs.length - 1] || {}).id || null;
    },

    setActor(key) {
      this.actor = this.actor === key ? '' : key;
    },

    setLifetime(key, uid) {
      this.actor = key;
      this.$nextTick(() => {
        this.lifetime = this.lifetime === uid ? '' : uid;
      });
    },

    setRange([a, b]) {
      this.a = a;
      this.b = b;
    },

    reset() {
      Object.assign(this, {
        a: 0, b: 1440, cluster: '', project: '', ns: '', actor: '', lifetime: ''
      });
    },

    applyBackend() {
      setBackendCluster(this.backendIn.trim());
      this.backend = backendCluster();
      this.load();
    },
  },
};
</script>

<template>
  <div class="al">
    <div class="head-stack">
      <div
        class="topbar"
        role="group"
        :aria-label="T.title"
      >
        <span class="crumb">Audit Loupe ／ {{ T.crumb }}</span>
        <h1>{{ T.title }}</h1>
        <input
          v-model="day"
          type="date"
          :aria-label="T.day"
        >
        <div class="filters">
          <select
            v-model="cluster"
            :class="{active: cluster}"
            :aria-label="T.cluster"
          >
            <option value="">
              {{ T.allClusters }}
            </option>
            <option
              v-for="[id, label] in clusterOptions"
              :key="id"
              :value="id"
            >
              {{ label }}
            </option>
          </select>
          <select
            v-model="project"
            :class="{active: project}"
            :aria-label="T.project"
          >
            <option value="">
              {{ T.allProjects }}
            </option>
            <option
              v-for="p in projectOptions"
              :key="p"
              :value="p"
            >
              {{ p }}
            </option>
          </select>
          <select
            v-model="ns"
            :class="{active: ns}"
            aria-label="Namespace"
          >
            <option value="">
              {{ T.allNs }}
            </option>
            <option
              v-for="n in nsOptions"
              :key="n"
              :value="n"
            >
              {{ n }}
            </option>
          </select>
          <select
            v-model="actor"
            :class="{active: actor}"
            :aria-label="T.whoDid"
          >
            <option value="">
              {{ T.everyone }}
            </option>
            <option
              v-for="[k, label] in actorOptions"
              :key="k"
              :value="k"
            >
              {{ label }}
            </option>
          </select>
          <label class="check"><input
            v-model="sys"
            type="checkbox"
          > {{ T.withSystem }}</label>
          <button
            class="btn-al"
            type="button"
            @click="reset"
          >
            {{ T.reset }}
          </button>
        </div>
      </div>

      <RecomputeBanner
        v-if="!missing"
        @done="load"
      />
    </div>
    <div
      v-if="missing || error"
      class="banner-al"
    >
      <span>{{ missing ? T.backendMissing : error }}</span>
      <template v-if="missing">
        <label class="check">{{ T.backendCluster }}
          <input
            v-model="backendIn"
            type="text"
            size="14"
          ></label>
        <button
          class="btn-al"
          type="button"
          @click="applyBackend"
        >
          {{ T.backendSave }}
        </button>
      </template>
    </div>

    <div
      v-else
      class="work"
    >
      <section class="col">
        <div class="col-head">
          <h2>{{ T.thisPeriod }}</h2><span class="muted num">{{ fmtMin(a) }}–{{ fmtMin(b) }}</span>
        </div>
        <div class="col-body">
          <SidePanel
            :T="T"
            :visible="visible"
            :base="inRangeScoped"
            :stories="visibleStories"
            :ACTORS="ACTORS"
            :OBJ="OBJ"
            :actor="actor"
            :lifetime="lifetime"
            :sel-event="selEvent"
            :loading="loading"
            :truncated="truncated"
            @select="select"
            @actor="setActor"
            @lifetime="setLifetime"
          />
        </div>
      </section>

      <section class="col">
        <div class="col-head">
          <h2>{{ T.changedObjects }}</h2>
          <div class="legend">
            <span><i
              class="lg"
              style="background:var(--al-added)"
            />{{ T.added }}</span>
            <span><i
              class="lg"
              style="background:var(--al-modified)"
            />{{ T.modified }}</span>
            <span><i
              class="lg"
              style="background:var(--al-deleted)"
            />{{ T.deleted }}</span>
            <span><i
              class="lg"
              style="background:var(--al-none)"
            />{{ T.readOnly }}</span>
            <span>{{ T.clickForRelations }}</span>
          </div>
        </div>
        <div
          ref="mapScroll"
          class="col-body"
        >
          <TopologyMap
            :T="T"
            :visible="visible"
            :OBJ="OBJ"
            :ACTORS="ACTORS"
            :sel="sel"
            :related="related"
            :coverage="coverage"
            @select="select"
          />
        </div>
      </section>

      <section
        class="col"
        aria-live="polite"
      >
        <div class="col-head">
          <h2>{{ T.evidence }}</h2><span class="muted">{{ T.evidenceHint }}</span>
        </div>
        <div class="col-body">
          <EvidencePanel
            :T="T"
            :obj="sel ? OBJ[sel] : null"
            :events="sel ? scoped.filter((e) => e.obj === sel) : []"
            :in-range="(e) => inRange(e) && matchesPerson(e)"
            :sel-event="selEvent"
            :ACTORS="ACTORS"
            :OBJ="OBJ"
            :related="related"
            @pick="(id) => selEvent = id"
            @select="select"
          />
        </div>
      </section>
    </div>

    <TimeAxis
      :T="T"
      :events="personScoped"
      :ACTORS="ACTORS"
      :OBJ="OBJ"
      :a="a"
      :b="b"
      :sel-event="selEvent"
      :work="work"
      :day="dayLabel"
      @range="setRange"
      @select="select"
    />
  </div>
</template>

<style lang="scss">
@import '../styles/change-map.scss';
</style>

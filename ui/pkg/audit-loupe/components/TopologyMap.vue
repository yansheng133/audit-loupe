<script>
import {
  netState, sevOf, worst, KIND_TAG
} from '../lib/model';

// Centre column: only objects touched in the range, grouped cluster -> project -> namespace.
// Relation lines are drawn for the selected object only.
export default {
  name:  'AuditLoupeTopologyMap',
  props: {
    T:        { type: Object, required: true },
    visible:  { type: Array, required: true },
    OBJ:      { type: Object, required: true },
    ACTORS:   { type: Object, required: true },
    sel:      { type: String, default: null },
    related:  { type: Array, default: () => [] },
    coverage: { type: Object, default: () => ({}) },
  },
  emits: ['select'],

  data() {
    return { paths: [], w: 0, h: 0 };
  },

  computed: {
    byObj() {
      const m = new Map();

      this.visible.forEach((e) => {
        if (!m.has(e.obj)) {
          m.set(e.obj, []);
        }
        m.get(e.obj).push(e);
      });

      return m;
    },
    relatedKeys() {
      return new Set(this.related.map((r) => r.key));
    },
    tree() {
      const t = {};

      [...this.byObj.keys()].forEach((k) => {
        const o = this.OBJ[k];

        (((t[o.cluster] ??= {})[o.project] ??= {})[o.ns] ??= []).push(k);
      });
      const special = [this.T.rancherSettings, this.T.clusterScope, this.T.noProject];

      return Object.keys(t).sort().map((c) => ({
        cluster:  c,
        label:    this.OBJ[[...this.byObj.keys()].find((k) => this.OBJ[k].cluster === c)].clusterLabel,
        projects: Object.keys(t[c])
          .sort((x, y) => (special.indexOf(x) - special.indexOf(y)) || x.localeCompare(y))
          .map((p) => ({
            name:  p,
            label: special.includes(p) ? p : `${ this.T.projectPrefix }${ p }`,
            nss:   Object.keys(t[c][p]).sort().map((n) => ({
              name: n,
              keys: t[c][p][n].sort((x, y) => this.OBJ[x].kind.localeCompare(this.OBJ[y].kind) || this.OBJ[x].name.localeCompare(this.OBJ[y].name)),
            })),
          })),
      }));
    },
  },

  watch: {
    sel:     'redraw',
    related: 'redraw',
    visible: 'redraw',
  },

  mounted() {
    this.ro = new ResizeObserver(() => this.redraw());
    this.ro.observe(this.$refs.map);
  },

  beforeUnmount() {
    this.ro?.disconnect();
  },

  methods: {
    chip(k) {
      const list = this.byObj.get(k);
      const o = this.OBJ[k];
      const sev = worst(list.map(sevOf));
      const low = sev === 'low' ? list.find((e) => sevOf(e) === 'low') : null;
      const last = [...list].sort((x, y) => y.ts - x.ts)[0];

      return {
        k,
        o,
        st:    netState(list),
        sev,
        flag:  sev ? (sev === 'low' ? (this.T.rules[low.alerts[0]] || low.alerts[0]) : this.T.risk(this.T.sev[sev])) : '',
        tag:   KIND_TAG[o.kind] || o.kind.slice(0, 6).toUpperCase(),
        dots:  [...new Set(list.map((e) => e.actor))].map((a) => this.ACTORS[a]),
        meta:  this.T.records(list.length, this.ACTORS[last.actor].label, last.hhmm),
      };
    },

    redraw() {
      this.$nextTick(() => requestAnimationFrame(() => this.drawLinks()));
    },

    // Same routing as the mock: curves between chips, around the right edge when stacked.
    drawLinks() {
      const map = this.$refs.map;

      if (!map) {
        return;
      }
      const mr = map.getBoundingClientRect();

      this.w = mr.width;
      this.h = mr.height;
      const chipOf = (k) => map.querySelector(`.chip[data-k="${ CSS.escape(k) }"]`);
      const a = this.sel && chipOf(this.sel);
      const out = [];

      if (a) {
        this.related.forEach((r) => {
          const b = chipOf(r.key);

          if (!b || r.key === this.sel) {
            return;
          }
          const [ea, eb] = r.direction === 'out' ? [a, b] : [b, a];
          const ra = ea.getBoundingClientRect();
          const rb = eb.getBoundingClientRect();
          let d; let lx; let ly;

          if (Math.abs(ra.left - rb.left) < 4) {
            const x = ra.right - mr.left;
            const y1 = ra.top + ra.height / 2 - mr.top;
            const y2 = rb.top + rb.height / 2 - mr.top;
            const bulge = 14 + Math.min(40, Math.abs(y2 - y1) / 6);

            d = `M${ x },${ y1 } C${ x + bulge },${ y1 } ${ x + bulge },${ y2 } ${ x },${ y2 }`;
            lx = x + bulge - 2;
            ly = (y1 + y2) / 2;
          } else {
            const leftFirst = ra.left < rb.left;
            const x1 = (leftFirst ? ra.right : ra.left) - mr.left;
            const x2 = (leftFirst ? rb.left : rb.right) - mr.left;
            const y1 = ra.top + ra.height / 2 - mr.top;
            const y2 = rb.top + rb.height / 2 - mr.top;
            const mx = (x1 + x2) / 2;

            d = `M${ x1 },${ y1 } C${ mx },${ y1 } ${ mx },${ y2 } ${ x2 },${ y2 }`;
            lx = mx;
            ly = (y1 + y2) / 2;
          }
          const label = this.T.rel[r.rel] || r.rel;

          out.push({
            d, x: lx + 4, y: ly + 4, label, bw: [...label].reduce((w, ch) => w + (ch.charCodeAt(0) > 255 ? 11 : 6.5), 6),
          });
        });
      }
      this.paths = out;
    },
  },
};
</script>

<template>
  <div
    ref="map"
    class="map"
    :class="{focus: !!sel && byObj.has(sel)}"
  >
    <svg
      class="links"
      :width="w"
      :height="h"
      aria-hidden="true"
    >
      <g
        v-for="(p, i) in paths"
        :key="i"
      >
        <path :d="p.d" />
        <rect
          class="lbl-bg"
          :x="p.x - 3"
          :y="p.y - 11"
          :width="p.bw"
          height="15"
          rx="3"
        />
        <text
          :x="p.x"
          :y="p.y"
        >{{ p.label }}</text>
      </g>
    </svg>
    <div
      v-if="!byObj.size"
      class="empty"
    >
      {{ T.empty }}
    </div>
    <div
      v-for="c in tree"
      :key="c.cluster"
      class="cluster"
    >
      <div class="cluster-head">
        <h3>{{ c.label }}</h3>
        <span
          v-if="c.label !== c.cluster"
          class="type"
        >{{ c.cluster }}</span>
        <span
          v-if="coverage[c.cluster]"
          class="cov"
        ><i :style="{background: `var(--al-${coverage[c.cluster].tone})`}" />{{ T.coverage }}{{ coverage[c.cluster].label }}</span>
      </div>
      <div class="projects">
        <div
          v-for="p in c.projects"
          :key="p.name"
          class="project"
        >
          <div class="project-name">
            {{ p.label }}
          </div>
          <div
            v-for="n in p.nss"
            :key="n.name"
            class="nsbox"
          >
            <div class="nsname">
              {{ n.name }}
            </div>
            <div class="chips">
              <button
                v-for="ch in n.keys.map(chip)"
                :key="ch.k"
                type="button"
                class="chip"
                :class="[ch.st, {sel: sel === ch.k, related: relatedKeys.has(ch.k)}]"
                :data-k="ch.k"
                @click="$emit('select', sel === ch.k ? null : ch.k)"
              >
                <span class="kindtag">{{ ch.tag }}</span>
                <span class="cname">{{ ch.o.name }}</span>
                <span
                  v-if="ch.sev"
                  class="flag"
                  :class="ch.sev"
                >{{ ch.flag }}</span>
                <span class="cmeta">
                  <span class="who-dots"><i
                    v-for="a in ch.dots"
                    :key="a.key"
                    :title="a.label"
                    :style="{background: a.color}"
                  /></span>
                  <span class="num">{{ ch.meta }}</span>
                </span>
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

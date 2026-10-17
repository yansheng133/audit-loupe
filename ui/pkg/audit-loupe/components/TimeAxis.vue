<script>
import { fmtMin, sevOf } from '../lib/model';

const TL = {
  L: 110, R: 20, axis: 16, hist: 22, gap: 4, lane: 16
};

// Bottom: the day as hourly volume plus one lane per person. Drag to choose the A–B range
// the rest of the page shows; A and B can also be dragged alone.
export default {
  name:  'AuditLoupeTimeAxis',
  props: {
    T:        { type: Object, required: true },
    events:   { type: Array, required: true },
    ACTORS:   { type: Object, required: true },
    OBJ:      { type: Object, required: true },
    a:        { type: Number, required: true },
    b:        { type: Number, required: true },
    selEvent: { type: String, default: null },
    work:     { type: Array, default: () => [9, 18] },
    day:      { type: String, default: '' },
  },
  emits: ['range', 'select'],

  data() {
    return {
      W: 900, drag: null, tip: null,
    };
  },

  computed: {
    lanes() {
      const seen = [];

      this.events.forEach((e) => {
        if (!seen.includes(e.actor)) {
          seen.push(e.actor);
        }
      });

      return seen;
    },
    histTop() {
      return TL.axis + 22;
    },
    lanesTop() {
      return this.histTop + TL.hist + TL.gap;
    },
    H() {
      return this.lanesTop + Math.max(this.lanes.length, 1) * TL.lane + 6;
    },
    ticks() {
      const out = [];

      for (let m = 0; m <= 1440; m += 180) {
        out.push({
          m, x: this.x(m), label: fmtMin(m), anchor: m === 0 ? 'start' : m === 1440 ? 'end' : 'middle'
        });
      }

      return out;
    },
    off() {
      const [s, e] = this.work;

      return [[0, s * 60], [e * 60, 1440]].filter(([p, q]) => q > p);
    },
    bars() {
      const buckets = Array.from({ length: 24 }, () => ({ n: 0, risky: 0 }));

      this.events.forEach((e) => {
        const bk = buckets[Math.max(0, Math.min(23, Math.floor(e.t / 60)))];

        bk.n++;
        if (['high', 'medium'].includes(sevOf(e))) {
          bk.risky++;
        }
      });
      const maxN = Math.max(1, ...buckets.map((x) => x.n));
      const bw = (this.x(60) - this.x(0)) - 3;

      return buckets.map((bk, i) => ({
        i, n: bk.n, x: this.x(i * 60) + 1.5, w: bw, h: (bk.n / maxN) * TL.hist, hr: (bk.risky / maxN) * TL.hist,
      })).filter((x) => x.n);
    },
    dots() {
      return this.events.map((ev) => {
        const y = this.lanesTop + this.lanes.indexOf(ev.actor) * TL.lane + TL.lane / 2;
        const o = this.OBJ[ev.obj];

        return {
          ev,
          cx:    this.x(ev.t),
          y,
          sev:   sevOf(ev),
          dim:   ev.t < this.a || ev.t > this.b,
          sel:   ev.id === this.selEvent,
          color: this.ACTORS[ev.actor].color,
          label: `${ ev.hhmm }　${ this.ACTORS[ev.actor].label } ${ ev.verb }・${ o.kind } ${ o.name }・${ this.T.via[ev.via] || ev.via }`,
        };
      });
    },
    presets() {
      const [s, e] = this.work;

      return [[0, 1440, this.T.allDay], [s * 60, e * 60, this.T.workHours(s, e)], [e * 60, 1440, this.T.afterHours(e)]];
    },
  },

  mounted() {
    this.ro = new ResizeObserver(() => {
      this.W = Math.max((this.$refs.wrap?.clientWidth || 900) - 4, 640);
    });
    this.ro.observe(this.$refs.wrap);
  },

  beforeUnmount() {
    this.ro?.disconnect();
  },

  methods: {
    fmtMin,
    x(m) {
      return TL.L + (m / 1440) * (this.W - TL.L - TL.R);
    },
    toMinutes(clientX) {
      const r = this.$refs.svg.getBoundingClientRect();
      const m = ((clientX - r.left) - TL.L) / (this.W - TL.L - TL.R) * 1440;

      return Math.max(0, Math.min(1440, Math.round(m / 5) * 5));
    },
    down(e) {
      const hnd = e.target.closest('[data-h]');
      const m = this.toMinutes(e.clientX);

      if (hnd) {
        this.drag = { mode: hnd.dataset.h };
      } else {
        this.drag = { mode: 'new', start: m };
        this.$emit('range', [m, Math.min(1440, m + 5)]);
      }
      this.$refs.svg.setPointerCapture(e.pointerId);
      this.tip = null;
    },
    move(e) {
      if (!this.drag) {
        return;
      }
      const m = this.toMinutes(e.clientX);

      if (this.drag.mode === 'A') {
        this.$emit('range', [Math.min(m, this.b - 10), this.b]);
      } else if (this.drag.mode === 'B') {
        this.$emit('range', [this.a, Math.max(m, this.a + 10)]);
      } else {
        this.$emit('range', [Math.min(this.drag.start, m), Math.max(this.drag.start, m)]);
      }
    },
    up() {
      if (!this.drag) {
        return;
      }
      this.drag = null;
      if (this.b - this.a < 10) {
        this.$emit('range', [this.a, Math.min(1440, this.a + 10)]);
      }
    },
    showTip(d) {
      this.tip = { text: d.label, left: Math.max(4, d.cx - 120), top: d.y - 34 };
    },
  },
};
</script>

<template>
  <section class="timeline">
    <div class="tl-head">
      <span class="range num">{{ T.rangeLabel(fmtMin(a), fmtMin(b)) }}</span>
      <span class="keys">{{ T.keys }}・{{ day }}</span>
      <div class="presets">
        <button
          v-for="[p, q, label] in presets"
          :key="label"
          type="button"
          class="btn-al"
          :class="{on: a === p && b === q}"
          @click="$emit('range', [p, q])"
        >
          {{ label }}
        </button>
      </div>
    </div>
    <div
      ref="wrap"
      class="tl-wrap"
    >
      <svg
        ref="svg"
        class="tl-svg"
        :width="W"
        :height="H"
        :viewBox="`0 0 ${W} ${H}`"
        @pointerdown="down"
        @pointermove="move"
        @pointerup="up"
        @pointercancel="up"
      >
        <rect
          v-for="([p, q]) in off"
          :key="p"
          class="offhours"
          :x="x(p)"
          :y="histTop - 4"
          :width="x(q) - x(p)"
          :height="H - histTop"
        />
        <template
          v-for="t in ticks"
          :key="t.m"
        >
          <line
            class="grid"
            :x1="t.x"
            :x2="t.x"
            :y1="histTop - 4"
            :y2="H - 4"
          />
          <text
            :x="t.x"
            y="12"
            :text-anchor="t.anchor"
          >{{ t.label }}</text>
        </template>
        <text
          x="4"
          :y="histTop + 15"
        >{{ T.hourly }}</text>
        <template
          v-for="bar in bars"
          :key="`h${bar.i}`"
        >
          <rect
            class="hist"
            :x="bar.x"
            :y="histTop + 22 - bar.h"
            :width="bar.w"
            :height="bar.h - bar.hr"
            rx="1.5"
          />
          <rect
            v-if="bar.hr"
            class="hist alert"
            :x="bar.x"
            :y="histTop + 22 - bar.hr"
            :width="bar.w"
            :height="bar.hr"
            rx="1.5"
          />
        </template>
        <text
          v-for="(k, i) in lanes"
          :key="`l${k}`"
          x="4"
          :y="lanesTop + i * 16 + 12"
          class="lane-label"
        >{{ ACTORS[k].label.length > 14 ? `${ACTORS[k].label.slice(0, 13)}…` : ACTORS[k].label }}</text>
        <rect
          class="brush"
          :x="x(a)"
          :y="histTop - 4"
          :width="Math.max(x(b) - x(a), 1)"
          :height="H - histTop"
        />
        <g
          v-for="([lab, m]) in [['A', a], ['B', b]]"
          :key="lab"
          :data-h="lab"
          style="cursor: ew-resize"
        >
          <rect
            class="handle"
            :x="x(m) - 7"
            :y="histTop - 18"
            width="14"
            height="15"
            rx="3"
          />
          <text
            class="hl"
            :x="x(m)"
            :y="histTop - 7"
            text-anchor="middle"
          >{{ lab }}</text>
          <rect
            :x="x(m) - 5"
            :y="histTop - 4"
            width="10"
            :height="H - histTop"
            fill="transparent"
          />
        </g>
        <g
          v-for="d in dots"
          :key="d.ev.id"
          class="dot"
          tabindex="0"
          role="button"
          :aria-label="d.label"
          :opacity="d.dim ? .3 : 1"
          @pointerenter="showTip(d)"
          @pointerleave="tip = null"
          @focus="showTip(d)"
          @blur="tip = null"
          @pointerdown.stop
          @click.stop="$emit('select', d.ev.obj, d.ev.id)"
          @keydown.enter.prevent="$emit('select', d.ev.obj, d.ev.id)"
        >
          <circle
            v-if="d.sev === 'high' || d.sev === 'medium'"
            :cx="d.cx"
            :cy="d.y"
            r="8.5"
            fill="none"
            stroke="var(--al-alert)"
            stroke-width="2"
          />
          <circle
            v-if="d.ev.change === 'deleted'"
            :cx="d.cx"
            :cy="d.y"
            :r="d.sel ? 6 : 5"
            fill="var(--al-surface)"
            :stroke="d.color"
            stroke-width="2.5"
          />
          <rect
            v-else-if="!d.ev.change"
            :x="d.cx - 4.5"
            :y="d.y - 4.5"
            width="9"
            height="9"
            :transform="`rotate(45 ${d.cx} ${d.y})`"
            :fill="d.color"
            stroke="var(--al-surface)"
            stroke-width="1.5"
          />
          <circle
            v-else
            :cx="d.cx"
            :cy="d.y"
            :r="d.sel ? 6.5 : 5"
            :fill="d.color"
            :stroke="d.sel ? 'var(--al-fg)' : 'var(--al-surface)'"
            stroke-width="1.5"
          />
          <circle
            v-if="d.sel"
            :cx="d.cx"
            :cy="d.y"
            r="11"
            fill="none"
            stroke="var(--al-fg)"
            stroke-width="1.2"
            stroke-dasharray="2 2"
          />
        </g>
      </svg>
      <div
        v-if="tip"
        class="tip"
        :style="{left: `${tip.left}px`, top: `${tip.top}px`}"
      >
        {{ tip.text }}
      </div>
    </div>
  </section>
</template>

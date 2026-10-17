<script>
import {
  sevOf, worst, isRisky, fmtDate
} from '../lib/model';

// Left column: totals, what needs attention (stories grouped by the server), who did what.
export default {
  name:  'AuditLoupeSidePanel',
  props: {
    T:         { type: Object, required: true },
    visible:   { type: Array, required: true },
    base:      { type: Array, required: true },
    stories:   { type: Array, required: true },
    ACTORS:    { type: Object, required: true },
    OBJ:       { type: Object, required: true },
    actor:     { type: String, default: '' },
    lifetime:  { type: String, default: '' },
    selEvent:  { type: String, default: null },
    loading:   { type: Boolean, default: false },
    truncated: { type: Boolean, default: false },
  },
  emits: ['select', 'actor', 'lifetime'],

  data() {
    return { expanded: {} };
  },

  computed: {
    stats() {
      return [
        [this.visible.length, this.T.ops],
        [new Set(this.visible.filter((e) => e.change).map((e) => e.obj)).size, this.T.objChanged],
        [this.visible.filter(isRisky).length, this.T.needAttention],
      ];
    },
    cards() {
      return this.stories.map((s) => {
        const evs = s.evs;
        const first = evs[0];
        const sev = worst(evs.map(sevOf)) || s.severity;
        const who = this.ACTORS[first.actor]?.label || s.actor;
        const span = Math.max(1, Math.round((evs[evs.length - 1].ts - first.ts) / 60000));
        const headline = evs.length > 1 ? this.T.storyBurst(who, span, evs.length) : this.headline(first);
        const why = evs.length > 1 ? evs.map((e) => `${ e.verb } ${ this.OBJ[e.obj].kind } ${ this.OBJ[e.obj].name }`).join('；') : [...new Set(first.alerts.map((r) => this.T.rules[r] || r))].join('、');

        return {
          id: s.id, sev, first, evs, headline, why, via: this.T.via[first.via] || first.via, sel: evs.some((e) => e.id === this.selEvent),
        };
      });
    },
    // Per person over the range, ignoring the person lens so you can switch lenses here.
    people() {
      const per = {};

      this.base.forEach((e) => {
        per[e.actor] = per[e.actor] || { n: 0, risky: 0 };
        per[e.actor].n++;
        if (isRisky(e)) {
          per[e.actor].risky++;
        }
      });
      const entries = Object.entries(per).sort((x, y) => y[1].n - x[1].n);
      const max = Math.max(1, ...entries.map(([, v]) => v.n));

      return entries.map(([k, v]) => ({
        key: k, ...v, pct: (v.n / max) * 100, a: this.ACTORS[k],
      }));
    },
  },

  methods: {
    fmtDate,
    headline(ev) {
      const o = this.OBJ[ev.obj];

      return `${ this.ACTORS[ev.actor].label } ${ ev.verb } ${ o.kind } ${ o.name }`;
    },
    toggle(k) {
      this.expanded = { ...this.expanded, [k]: !this.expanded[k] };
    },
  },
};
</script>

<template>
  <div class="side">
    <div
      v-if="loading"
      class="quiet"
    >
      {{ T.loading }}
    </div>
    <div
      v-if="truncated"
      class="quiet"
    >
      {{ T.truncated(2000) }}
    </div>
    <div class="stats">
      <div
        v-for="[v, k] in stats"
        :key="k"
        class="stat"
      >
        <div class="v num">
          {{ v }}
        </div>
        <div class="k">
          {{ k }}
        </div>
      </div>
    </div>

    <div class="sub">
      <span>{{ T.needAttention }}</span><span v-if="cards.length">{{ cards.length }} {{ T.items }}</span>
    </div>
    <div
      v-if="!cards.length"
      class="quiet"
    >
      {{ T.quiet }}
    </div>
    <div
      v-else
      class="att-list"
    >
      <button
        v-for="c in cards"
        :key="c.id"
        type="button"
        class="att"
        :class="[c.sev, {sel: c.sel}]"
        @click="$emit('select', c.first.obj, c.first.id)"
      >
        <span class="bar" />
        <div class="body">
          <div class="top">
            <span
              class="pill"
              :class="c.sev"
            >{{ T.risk(T.sev[c.sev]) }}</span>
            <span class="num">{{ c.evs.map((e) => e.hhmm).join('、') }}</span>
            <span>{{ c.via }}</span>
          </div>
          <span class="headline">{{ c.headline }}</span>
          <span class="why">{{ c.why }}</span>
        </div>
      </button>
    </div>

    <template v-if="people.length">
      <div class="sub">
        <span>{{ T.whoDid }}</span><span>{{ actor ? T.clickAgain : T.clickName }}</span>
      </div>
      <div class="people">
        <template
          v-for="p in people"
          :key="p.key"
        >
          <button
            type="button"
            class="person"
            :class="{on: actor === p.key && !lifetime}"
            :title="p.a.role"
            @click="$emit('actor', p.key)"
          >
            <span class="who"><i :style="{background: p.a.color}" />{{ p.a.label }}</span>
            <span class="bar"><i :style="{width: `${p.pct}%`, background: p.a.color}" /></span>
            <span class="num muted">{{ p.risky ? `${p.n}・${p.risky}` : p.n }}</span>
          </button>
          <div
            v-if="p.a.deleted || p.a.lifetimes.length > 1"
            class="lives"
          >
            <span>
              <span
                v-if="p.a.deleted"
                class="tag"
              >{{ T.deletedUser }}</span>
              <button
                v-if="p.a.lifetimes.length > 1"
                type="button"
                class="linkbtn"
                @click="toggle(p.key)"
              >{{ T.readded(p.a.lifetimes.length) }}・{{ expanded[p.key] ? T.merge : T.split }}</button>
            </span>
            <template v-if="expanded[p.key]">
              <button
                v-for="l in p.a.lifetimes"
                :key="l.uid"
                type="button"
                class="person"
                :class="{on: actor === p.key && lifetime === l.uid}"
                @click="$emit('lifetime', p.key, l.uid)"
              >
                <span class="muted num">{{ T.lifetime(fmtDate(l.created_ms), l.deleted_ms ? fmtDate(l.deleted_ms) : '') }}</span>
                <span />
                <span class="num muted">{{ l.writes }}</span>
              </button>
            </template>
          </div>
        </template>
      </div>
    </template>
    <p class="hint">
      {{ T.coverageHint }}
    </p>
  </div>
</template>

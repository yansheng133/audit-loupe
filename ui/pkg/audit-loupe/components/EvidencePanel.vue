<script>
import { identityChain, RULE_SEV } from '../lib/model';
import { baseUrl, getJson } from '../lib/api';

// Right column: every action on the selected object this day, then for the chosen one who
// did it (identity chain), what it changed, and the related objects.
export default {
  name:  'AuditLoupeEvidencePanel',
  props: {
    T:        { type: Object, required: true },
    obj:      { type: Object, default: null },
    events:   { type: Array, required: true },
    inRange:  { type: Function, required: true },
    selEvent: { type: String, default: null },
    ACTORS:   { type: Object, required: true },
    OBJ:      { type: Object, required: true },
    related:  { type: Array, default: () => [] },
  },
  emits: ['pick', 'select'],

  data() {
    return { flagged: null };
  },

  watch: {
    ev: {
      immediate: true,
      handler: 'loadFlagged',
    },
  },

  computed: {
    sorted() {
      return [...this.events].sort((a, b) => a.ts - b.ts);
    },
    anyOut() {
      return this.sorted.some((e) => !this.inRange(e));
    },
    ev() {
      return this.sorted.find((e) => e.id === this.selEvent) || null;
    },
    chain() {
      return this.ev ? identityChain(this.ev, this.obj, this.ACTORS[this.ev.actor], this.T) : null;
    },
    // The version this action produced; restoring means the state before it.
    restoreUrl() {
      const r = this.ev?.raw;
      const vid = r?.unattributed ? r.id : r?.version_id;

      return vid && this.ev.change && this.ev.change !== 'added' ? `${ baseUrl() }/api/restore?version_id=${ vid }` : null;
    },
    rel() {
      return this.related.map((r) => ({
        ...r,
        onMap: !!this.OBJ[r.key],
        text:  r.direction === 'out' ? `${ this.T.rel[r.rel] || r.rel } ${ r.kind } ${ r.name }` : `${ r.kind } ${ r.name }（${ this.T.rel[r.rel] || r.rel }）`,
      }));
    },
  },

  methods: {
    // For an exec or attach: processes SUSE Security flagged in that pod afterwards.
    async loadFlagged(ev) {
      this.flagged = null;
      const r = ev?.raw;

      if (!r || r.resource !== 'pods' || !['exec', 'attach'].includes(r.subresource)) {
        return;
      }
      try {
        const rows = await getJson('/api/security', {
          cluster: r.cluster, namespace: r.namespace, pod: r.name, from: new Date(ev.ts).toISOString()
        });

        if (this.ev === ev) {
          this.flagged = rows;
        }
      } catch (e) {
        this.flagged = null;
      }
    },
    fmtTs(ms) {
      const d = new Date(ms);

      return `${ String(d.getHours()).padStart(2, '0') }:${ String(d.getMinutes()).padStart(2, '0') }:${ String(d.getSeconds()).padStart(2, '0') }`;
    },
    sev(e, r) {
      return e.sev?.[r] || RULE_SEV[r] || 'low';
    },
  },
};
</script>

<template>
  <div class="detail">
    <div
      v-if="!obj"
      class="empty"
    >
      <strong>{{ T.pickTitle }}</strong>
      <span>{{ T.pickBody }}</span>
    </div>
    <template v-else>
      <div class="objhead">
        <h2>{{ obj.kind }} {{ obj.name }}</h2>
        <span class="path">{{ obj.clusterLabel }} ／ {{ obj.project }} ／ {{ obj.ns }}</span>
      </div>
      <div class="sub">
        {{ T.opsToday(sorted.length, anyOut) }}
      </div>
      <div class="steps">
        <button
          v-for="e in sorted"
          :key="e.id"
          type="button"
          class="step"
          :class="{sel: e.id === selEvent, out: !inRange(e)}"
          @click="$emit('pick', e.id)"
        >
          <span class="t num">{{ e.hhmm }}</span>
          <span class="rail"><i :style="{background: ACTORS[e.actor].color}" /></span>
          <span class="card">
            <span class="line"><span class="who"><i :style="{background: ACTORS[e.actor].color}" />{{ ACTORS[e.actor].label }}</span> {{ e.verb }}</span>
            <span class="pills">
              <span class="pill">{{ T.via[e.via] || e.via }}</span>
              <span
                v-if="e.change"
                class="pill"
                :class="e.change"
              >{{ T[e.change] }}</span>
              <span
                v-for="r in e.alerts"
                :key="r"
                class="pill"
                :class="sev(e, r)"
              >{{ T.rules[r] || r }}</span>
            </span>
          </span>
        </button>
      </div>

      <div
        v-if="ev"
        class="evidence"
      >
        <h3>{{ T.whoDidIt(ev.hhmm) }}</h3>
        <div class="chain">
          <template
            v-for="([k, v, warn], i) in chain.hops"
            :key="i"
          >
            <span
              v-if="i"
              class="arrow"
            >→</span>
            <div
              class="hop"
              :class="{warn}"
            >
              <span class="k">{{ k }}</span><span class="v">{{ v }}</span>
            </div>
          </template>
        </div>
        <p
          v-if="chain.note"
          class="chain-note"
        >
          {{ chain.note }}
        </p>
      </div>

      <div
        v-if="flagged"
        class="evidence"
      >
        <h3>{{ T.secTitle }}</h3>
        <div
          v-if="flagged.length"
          class="diff"
        >
          <div
            v-for="(f, i) in flagged"
            :key="i"
            class="ctx"
          >
            {{ fmtTs(f.tsMs) }} {{ f.process }}: {{ f.command }}（{{ f.parent }}）
          </div>
        </div>
        <p class="chain-note">
          {{ flagged.length ? T.secNote : T.secNone }}
        </p>
      </div>

      <div
        v-if="ev && (ev.diff.length || ev.note || rel.length || restoreUrl)"
        class="evidence"
      >
        <h3>{{ ev.diff.length ? T.whatChanged(ev.hhmm) : T.notes(ev.hhmm) }}</h3>
        <div
          v-if="ev.diff.length"
          class="diff"
        >
          <div
            v-for="([op, text], i) in ev.diff"
            :key="i"
            :class="op === '+' ? 'add' : op === '-' ? 'del' : 'ctx'"
          >
            {{ op }} {{ text }}
          </div>
        </div>
        <p
          v-if="ev.note"
          class="chain-note"
          :style="{paddingTop: ev.diff.length ? '0' : '10px'}"
        >
          {{ ev.note }}
        </p>
        <div
          v-if="restoreUrl"
          class="impact"
        >
          <a
            class="linkbtn"
            :href="restoreUrl"
            download
          >{{ T.restore }}</a>
          <span class="muted">{{ T.restoreHint }}</span>
        </div>
        <div
          v-if="rel.length"
          class="impact"
        >
          <span class="muted">{{ T.related }}</span>
          <button
            v-for="r in rel"
            :key="`${r.direction}${r.rel}${r.key}`"
            type="button"
            class="linkbtn"
            :class="{plain: !r.onMap}"
            :disabled="!r.onMap"
            @click="$emit('select', r.key)"
          >
            {{ r.text }}
          </button>
        </div>
      </div>
    </template>
  </div>
</template>

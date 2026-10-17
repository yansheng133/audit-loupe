<script>
import { fmtValue, dict } from '../lib/api';

const masked = (v) => typeof v === 'string' && v.startsWith('sha256:');

export default {
  name:  'ChangeList',
  props: { changes: { type: Array, default: () => [] } },
  data() {
    return { t: dict(this.$store) };
  },
  methods: {
    fmtValue,
    hidden(c) {
      return masked(c.old) || masked(c.new);
    },
  },
};
</script>

<template>
  <ul class="at-changes">
    <li
      v-for="c in changes"
      :key="c.path + c.op"
      :class="'op-' + c.op"
    >
      <code class="path">{{ c.path }}</code>
      <span
        v-if="hidden(c)"
        class="muted"
      >{{ c.op === 'changed' ? t.valueChanged : (c.op === 'added' ? t.added : t.deleted) }}（{{ t.valueHidden }}）</span>
      <template v-else-if="c.op === 'changed'">
        <span class="old">{{ fmtValue(c.old) }}</span>
        <span class="arrow">→</span>
        <span class="new">{{ fmtValue(c.new) }}</span>
      </template>
      <span
        v-else-if="c.op === 'added'"
        class="new"
      >+ {{ fmtValue(c.new) }}</span>
      <span
        v-else
        class="old"
      >− {{ fmtValue(c.old) }}</span>
    </li>
  </ul>
</template>

<style lang="scss" scoped>
.at-changes {
  list-style: none;
  margin: 0;
  padding: 0;
  li { margin: 2px 0; font-size: 13px; line-height: 1.5; word-break: break-all; }
  .path { margin-right: 8px; }
  .old { color: var(--error); text-decoration: line-through; }
  .op-removed .old { text-decoration: none; }
  .new { color: var(--success); }
  .arrow { margin: 0 6px; color: var(--muted); }
  .muted { color: var(--muted); }
}
</style>
